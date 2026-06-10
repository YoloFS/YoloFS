# Permission Gating Layer

The permission gating layer controls which files an agent can access and how.
Every path starts in the `ask` state. A rule engine promotes matching paths to
`allow`, `write-ask`, `read-only`, `ask`, `deny`, or `hide`. When an access
needs approval (`ask`, or a write under `write-ask`), the thread is put to
sleep; a userspace daemon receives the request and writes back a decision that
wakes the thread.

Permission gating applies to **file access** (open for read/write/exec)
and **metadata mutations** (create, mkdir, unlink, rmdir, rename,
symlink).  Directory mutations use the permission of the parent
directory.  Directory read-like operations (lookup/traversal, readdir,
stat) are **not** permission-gated — only `hide` applies (returns
ENOENT).  `deny` and `ask` have no effect on directory read-like ops.

## Permission States

```c
enum yolo_perm {
    YOLO_PERM_UNSET,       // No rule on this dentry (walk up to find one).
    YOLO_PERM_ASK,         // Default. Block thread, ask userspace.
    YOLO_PERM_ALLOW,       // Read + write + execute allowed.
    YOLO_PERM_WRITE_ASK,   // Read + execute; writes ask userspace.
    YOLO_PERM_READ_ONLY,   // Read + execute; writes denied.
    YOLO_PERM_DENY,        // Files: all access denied.
                           // Dirs: traversal/readdir/stat still work,
                           //       but mutations are denied.
    YOLO_PERM_HIDE,        // Like deny, but the path itself is invisible.
                           // Parent's readdir skips it; stat returns -ENOENT.
};
```

Operations passed in ask requests:

```c
enum yolo_op {
    YOLO_OP_READ  = 1,    // File opened for reading.
    YOLO_OP_WRITE = 2,    // File opened for writing (includes append/truncate).
};
```

## In-Kernel State

Permission state is organized by concern into three groups: rule storage,
per-inode state, and the ask protocol engine.

### Rule Storage

Rules live directly on dentries. One field, one structure.

**Per-dentry** (`yolo_dentry_info`) — one per cached dentry:

| Field | Purpose |
|-------|---------|
| `perm` | `YOLO_PERM_UNSET` unless this dentry has an explicit rule. Set or cleared by `YOLO_IOC_RULE_SET` (a perm of `UNSET` clears it). The root dentry also starts as `UNSET`; reaching the root without finding a rule means the built-in default `ask`. The dentry is pinned (via `dget`) while a rule is attached to prevent eviction. |

### Per-Superblock (`yolo_sb_info`)

| Field | Purpose |
|-------|---------|
| `perm.enabled` | Bool — whether permission gating is enabled. When false, all checks are skipped. |
| `perm.gen` | Atomic generation counter, starts at 1. Bumped on every rule add/remove/invalidation. Compared against per-inode `perm_gen` for O(1) staleness check. |

### Per-Inode (`yolo_inode_info`)

| Field | Purpose |
|-------|---------|
| `cached_perm` | Resolved permission (inherited from nearest ancestor rule). Cached at lookup time, re-resolved lazily when `perm_gen` is stale. |
| `perm_gen` | The `sbi->perm.gen` value when `cached_perm` was computed. If `!= sbi->perm.gen`, the cache is stale. |

### Ask Protocol Engine

The ask protocol handles operations whose effective policy asks for that
operation: reads or writes under `ask`, and writes under `write-ask`. The
thread sleeps until a userspace daemon decides. Ask state lives in
`struct yolo_permission` (embedded in `yolo_sb_info` as `perm`), plus one
`struct yolo_ask` per in-flight ask.

**Ask state** (`yolo_permission`) — embedded in `yolo_sb_info`:

| Field | Purpose |
|-------|---------|
| `pending_reqs` | Linked list of `yolo_ask` structs waiting to be dequeued by the daemon |
| `dispatched` | Linked list of requests handed to the daemon but not yet answered |
| `pending_lock` | Spinlock protecting both `pending_reqs` and `dispatched` |
| `request_waitq` | Wait queue — daemon's `GET_ASK` ioctl blocks here |
| `next_req_id` | Atomic counter for unique request IDs |
| `timeout_s` | Seconds to wait for an answer before denying (0 = infinite) |
| `daemon_file` | Pointer to the daemon's open `struct file` (a directory fd in the mount); NULL if no daemon connected (NULL is itself the "connected" flag). Set atomically on the first `GET_ASK` ioctl, cleared in `yolo_ctl_release()`. Only one daemon allowed — a second `GET_ASK` from a different fd returns `-EBUSY`. |

The daemon connects by opening the mount root (a directory fd) and issuing its
first `GET_ASK` ioctl to claim exclusive daemon status. On close, all
dispatched-but-unanswered requests are denied and `daemon_file` is reset to
NULL.

Control ioctls live on a directory fd in the mount (there is no separate `.ctl`
file). Operations that could defeat gating — `RULE_SET`, `GET_ASK`,
`PUT_DECISION` — are refused when the caller is chrooted *inside* the mount (a
command run via `yolo run -- <cmd>`), so nothing running inside the
mount can un-gate itself or answer its own ask prompts. `RESTORE` and `TRAVEL`
are also refused because their serialized trees can redirect a visible name
to an arbitrary host path. `SNAPSHOT` and `RULE_RESOLVE` remain allowed from
inside. This is the real boundary;
on top of it the CLI refuses to run *any* `yolo` command from inside the mount
(it is a host-side tool — `review`/`commit` need the base filesystem, which
only exists outside).

**Per-ask** (`yolo_ask`) — one per in-flight ask:

| Field | Purpose |
|-------|---------|
| `id` | Unique request ID (from `next_req_id`) |
| `access_path`, `op`, `pid`, `comm` | Access context sent to the daemon |
| `rule_path`, `rule_perm` | Source rule context; `rule_path` is empty for the built-in default `ask` |
| `decision` | Allow/deny decision set by the daemon's `PUT_DECISION` ioctl |
| `done` | Completion — the blocked thread sleeps here |
| `ref` | Refcount (kernel thread + daemon fd each hold a ref) |

## Rule Engine

Rules are **attached to dentries**. Resolved permissions are **cached on
inodes** with a **generation counter** for cheap invalidation.

Two levels:
- **Dentry**: `perm` field — only set on dentries that have an explicit rule
  (`YOLO_PERM_UNSET` otherwise). Rules are pinned so the dentry is never evicted.
- **Inode**: `cached_perm` + `perm_gen` — resolved permission cached during
  `lookup()` by inheriting from the nearest ancestor dentry with a rule.
  Checked in `permission()` with O(1) cost.

**Setting a rule** (`yolo rule allow src`):

1. Write the rule to `yolofs.toml` (source of truth on disk):
  ```toml
   prompt_timeout = 30

   [rules]
   "src"        = "allow"
   "/etc"       = "write-ask"
   "/etc/hosts" = "read-only"
   "/usr/bin"   = "read-only"
   "/secret"    = "hide"
  ```

   Paths can be **absolute** (`/etc`) or **relative** to the session root:
   the directory containing `.yolofs/` (equivalently, the CWD where `yolo`
   was launched). For example, `src` resolves to
   `/home/user/project/src`.
2. If a mount exists (`.yolofs/mnt` is mounted), also apply live:
   `ioctl(YOLO_IOC_RULE_SET)` -> kernel resolves the normalized absolute path
   to a dentry, sets `YOLO_D(dentry)->perm`, pins the dentry, and bumps
   `perm_gen` to invalidate all cached inode perms.

If no mount exists, the rule is persisted to `yolofs.toml` only. It will be
applied on the next `yolo mount`.

On mount, the CLI reads `yolofs.toml` and applies all `[rules]` via ioctl.

**Changing a rule**: just set it again + bump generation.

**Removing a rule** (`yolo rule unset /foo/bar`):

1. Remove the rule from `yolofs.toml`.
2. If a mount exists, also apply live:
   `ioctl(YOLO_IOC_RULE_SET)` with perm `UNSET` -> kernel sets
   `YOLO_D(dentry)->perm = UNSET`, unpins the dentry, and bumps `perm_gen`.

**Permission resolution — cached on inode, resolved lazily**:

```c
// Resolve by walking up dentry chain (only called on cache miss).
enum yolo_perm yolo_resolve_perm(struct dentry *dentry)
{
    struct dentry *cur = dentry;
    while (cur) {
        struct yolo_dentry_info *di = YOLO_D(cur);
        if (di && di->perm != YOLO_PERM_UNSET)
            return di->perm;
        if (cur == cur->d_parent)
            break;              // reached root dentry
        cur = cur->d_parent;
    }
    return YOLO_PERM_ASK;   // built-in default; no rule path
}

// Called during lookup() -- cache the resolved perm on the inode.
void yolo_cache_perm(struct inode *inode, struct dentry *dentry)
{
    struct yolo_inode_info *info = YOLO_I(inode);
    struct yolo_sb_info *sb = YOLO_SB(inode->i_sb);

    info->cached_perm = yolo_resolve_perm(dentry);
    info->perm_gen = atomic64_read(&sb->perm.gen);
}

// Shared permission check: resolve, ask if needed, check the requested op.
// Used by yolo_open (for file access) and yolo_check_mutate_perm (for
// metadata ops).  Lives in perm.c.
int yolo_check_dentry_perm(struct yolo_sb_info *sbi,
                           struct dentry *dentry,
                           int f_flags)
{
    struct yolo_inode_info *ii = YOLO_I(d_inode(dentry));
    enum yolo_perm perm;
    enum yolo_decision decision;
    enum yolo_op op = yolo_open_op(f_flags);

    if (ii->perm_gen != atomic64_read(&sbi->perm.gen))
        yolo_cache_perm(d_inode(dentry), dentry);
    perm = ii->cached_perm;

    if (perm == YOLO_PERM_ASK ||
        (perm == YOLO_PERM_WRITE_ASK && op == YOLO_OP_WRITE)) {
        // ... ask daemon, or deny if none answers; fills decision ...
        return decision == YOLO_DECISION_ALLOW ? 0 : -EACCES;
    }
    return yolo_check_perm(perm, f_flags);
}

// Metadata ops check write permission on the parent directory.
static int yolo_check_mutate_perm(struct dentry *dentry)
{
    struct yolo_sb_info *sbi = YOLO_SB(dentry->d_sb);
    if (!sbi->perm.enabled)
        return 0;
    return yolo_check_dentry_perm(sbi, dentry->d_parent, O_WRONLY);
}

// yolo_permission() for VFS MAY_READ/MAY_WRITE/MAY_EXEC checks.
// Directories do not sleep here; directory read-like ops only check `hide`.
static int yolo_permission(struct mnt_idmap *idmap,
                           struct inode *inode, int mask)
{
    // ... regular files enforce allow/deny here ...
}
```

The root dentry starts as `YOLO_PERM_UNSET`. Reaching the root without finding
an explicit rule returns the built-in default `YOLO_PERM_ASK`; an explicit
`/ = "ask"` rule is represented as a real root-dentry rule and is reported to
userspace as `rule_path = "/"`. In steady state (no rule changes),
`permission()` is a single generation compare + switch — O(1). On rule change,
the generation bumps and inodes re-resolve lazily on next access.

The `ask` path is handled in operations that have a stable dentry and may
sleep: `yolo_open()` for regular-file opens.  Directory read-like
operations (readdir, lookup/traversal, stat) are **not** gated by ask —
they only check for `hide`:

```c
static int yolo_open(struct inode *inode, struct file *file)
{
    struct dentry *dentry = file->f_path.dentry;

    if (sbi->perm.enabled) {
        err = yolo_check_dentry_perm(sbi, dentry, file->f_flags);
        if (err)
            return err;
    }
    // ... staging redirect (lazy COW, see staging.md#open--read--write-path) ...
}
```

Metadata operations (create, mkdir, unlink, rmdir, rename, symlink) use
the same check on the **parent directory** to verify write permission:

```c
static int yolo_create(struct mnt_idmap *idmap, struct inode *dir,
                       struct dentry *dentry, umode_t mode, bool excl)
{
    int err = yolo_check_mutate_perm(dentry);  // checks parent for write
    if (err)
        return err;
    return yolo_create_staged(dir, dentry, mode, NULL);
}
```

**Example**:

```bash
 yolo rule allow src
 yolo rule write-ask /etc
 yolo rule read-only /etc/hosts
 yolo rule read-only /usr/bin
 yolo rule hide  ~/.mozilla
```

- `permission("src/main.rs")` -> cached_perm=ALLOW (from lookup) -> **pass**
- `permission("etc/passwd")` -> cached_perm=WRITE_ASK -> **pass for read/exec, ask on write**
- `permission("etc/hosts")` -> cached_perm=READ_ONLY -> **pass for read/exec, deny write**
- `readdir("etc")` -> cached_perm=WRITE_ASK -> **pass** (dir read-like ops not gated)
- `stat("etc")` -> cached_perm=WRITE_ASK -> **pass** (dir read-like ops not gated)
- `open("tmp/foo")` -> cached_perm=ASK -> ask daemon -> **sleeps until decision**

## The Ask Protocol

When a thread accesses a file whose effective permission is `ask`, or writes a
file whose effective permission is `write-ask`:

```
  Thread (kernel)                          Daemon (userspace)
  ──────────────                           ──────────────────
  1. yolo_check_perm() -> perm asks for this op
  2. Allocate yolo_ask {
       id, access_path, rule_path, rule_perm, op, pid, comm
     }
  3. Enqueue request on sb->pending_reqs
  4. wake_up(&sb->request_waitq)
  5. wait_event_interruptible(              ioctl(GET_ASK) blocks
       req->done                             until request is available
       (completion)                          |
     )                                      dequeue request
     ...thread sleeps...                     -> struct yolo_ioc_ask {
                                                       id, access_path,
                                                       rule_path, rule_perm, op, ...
                                                    }
                                             |
                                            Daemon shows prompt / decides
                                             |
                                             ioctl(PUT_DECISION) -> struct yolo_ioc_decision {
                                                         id: 42, decision: ALLOW }
                                              |
   6. req->decision = ALLOW                  ioctl handler:
   7. complete(&req->done)                     find request by id
     ...thread wakes...                       set decision
  8. Proceed/fail this operation             complete(&req->done)
```

Key properties:

- **Interruptible sleep**: The thread can be killed with `SIGKILL`. The
  request is removed from the pending list and `-EINTR` is returned.
- **Timeout**: Configurable via mount option `prompt_timeout=<seconds>`.
  If the daemon doesn't respond in time, the request is denied.
- **Minimal response**: `yolo_ioc_decision` only carries `{ id, decision }`.
  Valid decisions are always `allow` or `deny`; they answer only the current
  blocked operation.
  Persisting policy is always a separate `ioctl(YOLO_IOC_RULE_SET)`.
  `hide` is rule-only: a hidden path returns `ENOENT` and never issues an ask,
  because prompting would already disclose the path.
- **One-shot decisions**: Ask decisions do not mutate the cached rule mode. An
  `allow` decision lets only the current access proceed; a later access asks
  again unless userspace separately calls `ioctl(YOLO_IOC_RULE_SET)` to install
  a persistent rule. `write-ask` likewise keeps asking on later writes.

## Why Dentry Walk-Up?

The rule engine must satisfy these design principles:

1. **Fast checks** — permission resolution must not scale with the number of
   rules. O(n) scanning per access is unacceptable.
2. **Hierarchical rules** — a single rule on a directory applies to all files
   underneath. Rules can overlap (e.g., `/etc` = write-ask, `/etc/hosts` = read-only)
   and the most specific path always wins, regardless of insertion order.
3. **Dynamic rules** — adding, changing, or removing a rule must take effect
   immediately without expensive cache invalidation.

These principles rule out most alternatives:

| Approach | Violates |
|---|---|
| Sorted array scan | #1 — O(n) per access |
| First-match glob list | #1 — O(n), #2 — order-dependent |
| Dentry-cached inheritance | #3 — rule change requires flushing children |
| Per-file hashtable | #2 — no subtree support without enumerating all files |

The **dentry tree is already a path-component trie**. Walking `d_parent` is
longest-prefix-match for free. This satisfies all three principles:

1. O(depth) — typically 3-8 pointer hops, independent of rule count.
2. Walk finds the nearest ancestor with a rule — subtrees, overlaps, and
   per-file overrides all fall out naturally from bottom-up traversal.
3. Just set `dentry->perm` — no child invalidation, immediate effect.

## Cache Invalidation

- On rule add/remove: `atomic_inc(&sb->perm.gen)`. All inode caches go
  stale; next `permission()` call re-resolves lazily via `d_find_alias()` +
  walk up. O(1) invalidation.
- On `YOLO_IOC_RESTORE` (including the empty-tree commit/abort case) and
  `YOLO_IOC_TRAVEL`:
  bumps perm_gen and shrinks the dentry cache, so permission re-resolution picks
  up changes.
- On `rename`: pure renames do **not** bump `perm_gen`. The inode keeps its
  `cached_perm` until some later invalidation event (rule add/remove or
  `YOLO_IOC_TRAVEL`). This is intentional: rename is treated as a path
  move, not an immediate permission re-resolution point. A file moved from
  `src` under `/etc` may therefore continue to use its pre-rename effective
  permission until the next generation bump. This trades strict
  post-rename freshness for O(1) steady-state checks.

**What is gated**:

| Operation | Check | Gate point |
|-----------|-------|------------|
| open (read/write/exec) | file's own perm | `yolo_open` → `yolo_check_dentry_perm` |
| readdir | hidden only | `yolo_dir_open` (inline hidden check) |
| stat | hidden only | `yolo_getattr` (inline hidden check) |
| lookup / traversal | not gated | — |
| create, mkdir, symlink | parent dir's perm (write) | `yolo_check_mutate_perm` |
| unlink, rmdir | parent dir's perm (write) | `yolo_check_mutate_perm` |
| rename | both parents' perm (write) | `yolo_check_mutate_perm` × 2 |

Whenever a gate returns `-EACCES`, the kernel appends a `B\0<path>\0<op>\n`
record to the journal (the *target* path the agent tried to act on, not
the parent whose perm was the source of denial; `op` is `r`/`w`). And
whenever an `ask` is resolved — by the daemon or the timeout default — the
kernel appends an `A\0<access_path>\0<op>\0<decision>\n` record capturing
the verdict. A records carry the attempted access path, not the rule path.
`yolo review` summaries and `yolo journal` surface both so the user can review
what was blocked or asked, in order, relative to snapshots. `HIDE` paths return
`-ENOENT`, never issue asks, and are not logged. See
[staging.md §Journal Format](staging.md#journal-format) for the record
shape and semantics.

**What is NOT gated**:

| Operation | Reason |
|-----------|--------|
| readlink | Symlink target; no open |

Under `deny`, an agent can see what files exist (names, sizes,
timestamps) and can traverse directories, but cannot read file contents,
modify, create, or delete.

### Hidden

`hide` goes further than `deny`: it makes the path itself invisible.
The parent directory's readdir skips hidden entries, and any access
(stat, open, lookup) returns ENOENT as if the path doesn't exist.

**Motivation**: `deny` is sufficient for preventing unauthorized
reads and writes, but it leaks the directory structure. An agent
under `deny` on `~/.mozilla` can still enumerate profile directories,
discover cached site favicons and history database filenames — enough
to infer which websites the user visits, purely from directory
listings. Similarly, `~/tax2025/w2.pdf` and `~/tax2025/1099-broker.pdf`
reveal financial information from filenames alone.
`hide` prevents this information leakage entirely: the path doesn't
exist from the agent's perspective.

**When to use each**:

| Level | Use case |
|-------|----------|
| `deny` | Directories the agent knows about but shouldn't modify (e.g., system config). Structure is visible for context. |
| `hide` | Personal data the agent has no reason to access (e.g., `~/tax2025`, `~/.mozilla`, `~/.local/share/keyrings`, medical records, financial documents). |

In `yolofs.toml`:

```toml
[rules]
"/usr"           = "read-only"
"/home/user/src" = "allow"
"/home/user/tax2025"          = "hide"
"/home/user/.mozilla"         = "hide"
```

## Comparison with Landlock

Landlock is a Linux Security Module (LSM) for unprivileged process
sandboxing. It shares the goal of path-based access control but differs
significantly in design.

**Rule interface**: Landlock uses file descriptors to identify paths. The
userspace process opens a path with `O_PATH`, passes the fd to
`landlock_add_rule()`, and the kernel resolves it to an inode. Rules follow
the inode, not the name — immune to rename attacks. YoloFS uses path strings
resolved to dentries; rules are name-based and stay on the dentry.

**Rule storage**: Landlock stores rules in an rb-tree keyed by inode object
pointer, one tree per ruleset. On access, it walks up every ancestor of the
target path and does an rb-tree lookup for each — O(depth x log n). YoloFS
stores rules directly on dentries and caches the resolved permission on
inodes with a generation counter — O(1) in steady state.

**Overlapping rules**: Landlock is additive — rules only grant permissions.
If `/foo` has no rule and `/foo/bar` grants read access, then `/foo/bar`
is readable but `/foo/baz` is denied. However, you **cannot** deny a child
when a parent is allowed: if `/foo` grants read access, then `/foo/bar` also
gets read access and there is no way to revoke it. YoloFS uses
nearest-ancestor wins: `/foo = allow` + `/foo/bar = deny` works because
the walk-up finds `/foo/bar`'s rule first. Both directions (allow parent
deny child, deny parent allow child) are supported.

**Dynamic rules**: Landlock rulesets are immutable once enforced via
`landlock_restrict_self()`. You cannot add or remove rules at runtime.
YoloFS rules can be added, changed, or removed at any time via ioctl, with
O(1) invalidation via generation counter.

**Default policy**: Landlock is deny-by-default for "handled" access rights.
YoloFS is ask-by-default — unmatched paths trigger the ask protocol, which
blocks the thread until a daemon decides.

**Scope**: Landlock is per-process (attached to credentials, inherited by
children). YoloFS is per-mount (all processes inside the mount share the same
rules and staging area).

| Aspect | Landlock | YoloFS |
|---|---|---|
| Rule target | fd -> inode (follows renames) | path -> dentry (name-based) |
| Rule storage | rb-tree per ruleset | `perm` field on dentry |
| Access check | O(depth x log n) per ancestor | O(1) via inode cache + gen counter |
| Overlap support | Additive only (can't deny child of allowed parent) | Nearest-ancestor wins (both directions) |
| Dynamic rules | No (immutable after enforce) | Yes (add/remove/change anytime) |
| Default | Deny (handled rights) | Ask (block + prompt) |
| Scope | Per-process (cred-attached) | Per-mount |
| Staging | N/A | Full commit/abort staging layer |

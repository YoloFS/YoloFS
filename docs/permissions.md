# Permission Gating Layer

The permission gating layer controls which files an agent can access and how.
Every path starts in the `ask` state. A rule engine promotes matching paths
to `allow`, `read`, `deny`, or `hide`. When a thread touches an `ask` file,
the thread is put to sleep; a userspace daemon receives the request and
writes back a decision that wakes the thread.

Permission gating applies to **file access** (open for read/write/exec)
and **metadata mutations** (create, mkdir, unlink, rmdir, rename,
symlink).  Directory mutations use the permission of the parent
directory.  Directory read-like operations (lookup/traversal, readdir,
stat) are **not** permission-gated — only `hide` applies (returns
ENOENT).  `deny` and `ask` have no effect on directory read-like ops.

## Permission States

```c
enum yolo_perm {
    YOLO_PERM_NONE,        // No rule on this dentry (walk up to find one).
    YOLO_PERM_ASK,         // Default. Block thread, ask userspace.
    YOLO_PERM_ALLOW,       // Read + write + execute allowed.
    YOLO_PERM_READ,        // Read + execute. No write.
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
    YOLO_OP_EXEC  = 3,    // File opened for execution.
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
| `perm` | `YOLO_PERM_NONE` unless this dentry has an explicit rule. Set by `YOLO_IOC_RULE_ADD`, cleared by `YOLO_IOC_RULE_REMOVE`. The dentry is pinned (via `dget`) while a rule is attached to prevent eviction. |

### Per-Superblock (`yolo_sb_info`)

| Field | Purpose |
|-------|---------|
| `permission` | Bool — whether permission gating is enabled. When false, all checks are skipped. |
| `perm_gen` | Atomic generation counter, starts at 1. Bumped on every rule add/remove/invalidation. Compared against per-inode `perm_gen` for O(1) staleness check. |

### Per-Inode (`yolo_inode_info`)

| Field | Purpose |
|-------|---------|
| `cached_perm` | Resolved permission (inherited from nearest ancestor rule). Cached at lookup time, re-resolved lazily when `perm_gen` is stale. |
| `perm_gen` | The `sbi->perm_gen` value when `cached_perm` was computed. If `!= sbi->perm_gen`, the cache is stale. |

### Ask Protocol Engine

The ask protocol handles paths with no matching rule. A thread accessing
an `ask` path sleeps until a userspace daemon decides. All ask state is
grouped into `struct yolo_ask_engine` (embedded in `yolo_sb_info` as
`ask_engine`), plus per-connection and per-request structures.

**Ask engine** (`yolo_ask_engine`) — embedded in `yolo_sb_info`:

| Field | Purpose |
|-------|---------|
| `pending_reqs` | Linked list of `yolo_perm_request` structs waiting for a daemon decision |
| `pending_lock` | Spinlock protecting `pending_reqs` |
| `request_waitq` | Wait queue — daemon's `GET_REQUEST` ioctl blocks here |
| `next_req_id` | Atomic counter for unique request IDs |
| `timeout_s` | Seconds before an unanswered ask applies the default |
| `default_perm` | Default decision (`deny` or `read`) when no daemon or timeout |
| `daemon_file` | Pointer to the daemon's open `struct file` (the `.ctl` control file); NULL if no daemon connected. Set atomically on the first `GET_REQUEST` ioctl, cleared in `yolo_ctl_release()`. Only one daemon allowed — a second `GET_REQUEST` from a different fd returns `-EBUSY`. |
| `dispatched` | Linked list of requests sent to daemon but not yet answered |
| `dispatch_lock` | Spinlock protecting `dispatched` and `daemon_file` |

The daemon connects by opening `.yolofs/mnt/.ctl` and issuing its first
`GET_REQUEST` ioctl to claim exclusive daemon status. On close, all
dispatched-but-unanswered
requests receive `default_perm` and `daemon_file` is reset to NULL.

**Per-request** (`yolo_perm_request`) — one per in-flight ask:

| Field | Purpose |
|-------|---------|
| `id` | Unique request ID (from `next_req_id`) |
| `path`, `op`, `pid`, `comm` | Context sent to the daemon |
| `decision` | Set by the daemon's `PUT_RESPONSE` ioctl |
| `done` | Completion — the blocked thread sleeps here |
| `ref` | Refcount (kernel thread + daemon fd each hold a ref) |

## Rule Engine

Rules are **attached to dentries**. Resolved permissions are **cached on
inodes** with a **generation counter** for cheap invalidation.

Two levels:
- **Dentry**: `perm` field — only set on dentries that have an explicit rule
  (`YOLO_PERM_NONE` otherwise). Rules are pinned so the dentry is never evicted.
- **Inode**: `cached_perm` + `perm_gen` — resolved permission cached during
  `lookup()` by inheriting from the nearest ancestor dentry with a rule.
  Checked in `permission()` with O(1) cost.

**Setting a rule** (`yolo rule add src allow`):

1. Write the rule to `yolofs.toml` (source of truth on disk):
  ```toml
   ask_timeout = 30
   ask_default = "deny"

   [rules]
   "src"        = "allow"
   "/etc"       = "deny"
   "/etc/hosts" = "read"
   "/usr/bin"   = "read"
   "/secret"    = "hide"
  ```

   Paths can be **absolute** (`/etc`) or **relative** to the session root:
   the directory containing `.yolofs/` (equivalently, the CWD where `yolo`
   was launched). For example, `src` resolves to
   `/home/user/project/src`.
2. If a mount exists (`.yolofs/mnt` is mounted), also apply live:
   `ioctl(YOLO_IOC_RULE_ADD)` -> kernel resolves the normalized absolute path
   to a dentry, sets `YOLO_D(dentry)->perm`, pins the dentry, and bumps
   `perm_gen` to invalidate all cached inode perms.

If no mount exists, the rule is persisted to `yolofs.toml` only. It will be
applied on the next `yolo mount`.

On mount, the CLI reads `yolofs.toml` and applies all `[rules]` via ioctl.

**Changing a rule**: just set it again + bump generation.

**Removing a rule** (`yolo rule remove /foo/bar`):

1. Remove the rule from `yolofs.toml`.
2. If a mount exists, also apply live:
   `ioctl(YOLO_IOC_RULE_REMOVE)` -> kernel sets `YOLO_D(dentry)->perm = NONE`,
   unpins the dentry, and bumps `perm_gen`.

**Permission resolution — cached on inode, resolved lazily**:

```c
// Resolve by walking up dentry chain (only called on cache miss).
enum yolo_perm yolo_resolve_perm(struct dentry *dentry)
{
    struct dentry *cur = dentry;
    while (cur) {
        struct yolo_dentry_info *di = YOLO_D(cur);
        if (di && di->perm != YOLO_PERM_NONE)
            return di->perm;
        if (cur == cur->d_parent)
            break;              // reached root dentry
        cur = cur->d_parent;
    }
    return YOLO_PERM_ASK;
}

// Called during lookup() -- cache the resolved perm on the inode.
void yolo_cache_perm(struct inode *inode, struct dentry *dentry)
{
    struct yolo_inode_info *info = YOLO_I(inode);
    struct yolo_sb_info *sb = YOLO_SB(inode->i_sb);

    info->cached_perm = yolo_resolve_perm(dentry);
    info->perm_gen = atomic64_read(&sb->perm_gen);
}

// Shared permission check: resolve, ask if needed, check the requested op.
// Used by yolo_open (for file access) and yolo_check_mutate_perm (for
// metadata ops).  Lives in perm.c.
int yolo_check_dentry_perm(struct yolo_sb_info *sbi,
                           struct dentry *dentry,
                           int f_flags, fmode_t f_mode)
{
    struct yolo_inode_info *ii = YOLO_I(d_inode(dentry));
    enum yolo_perm perm;

    if (ii->perm_gen != atomic64_read(&sbi->perm_gen))
        yolo_cache_perm(d_inode(dentry), dentry);
    perm = ii->cached_perm;

    if (perm == YOLO_PERM_ASK) {
        // ... ask daemon or apply default_perm ...
    }
    return yolo_check_perm(perm, f_flags);
}

// Metadata ops check write permission on the parent directory.
static int yolo_check_mutate_perm(struct dentry *dentry)
{
    struct yolo_sb_info *sbi = YOLO_SB(dentry->d_sb);
    if (!sbi->permission)
        return 0;
    return yolo_check_dentry_perm(sbi, dentry->d_parent, O_WRONLY, 0);
}

// yolo_permission() for VFS MAY_READ/MAY_WRITE/MAY_EXEC checks.
// Directories do not sleep here; ask handling for directory read-like ops
// lives in yolo_lookup/yolo_getattr/yolo_dir_open.
static int yolo_permission(struct mnt_idmap *idmap,
                           struct inode *inode, int mask)
{
    // ... regular files enforce allow/deny here ...
}
```

The root dentry has `perm = YOLO_PERM_ASK`. In steady state (no rule
changes), `permission()` is a single generation compare + switch — O(1).
On rule change, the generation bumps and inodes re-resolve lazily on
next access.

The `ask` path is handled in operations that have a stable dentry and may
sleep: `yolo_open()` for regular-file opens.  Directory read-like
operations (readdir, lookup/traversal, stat) are **not** gated by ask —
they only check for `hide`:

```c
static int yolo_open(struct inode *inode, struct file *file)
{
    struct dentry *dentry = file->f_path.dentry;

    if (sbi->permission) {
        err = yolo_check_dentry_perm(sbi, dentry, file->f_flags, file->f_mode);
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
 yolo rule add src          allow
 yolo rule add /etc         deny
 yolo rule add /etc/hosts   read
 yolo rule add /usr/bin     read
 yolo rule add ~/.mozilla   hide
```

- `permission("src/main.rs")` -> cached_perm=ALLOW (from lookup) -> **pass**
- `permission("etc/passwd")` -> cached_perm=DENY -> **-EACCES**
- `permission("etc/hosts")` -> cached_perm=READ -> **pass for read/exec, deny write**
- `readdir("etc")` -> cached_perm=DENY -> **pass** (dir read-like ops not gated)
- `stat("etc")` -> cached_perm=ASK -> **pass** (dir read-like ops not gated)
- `open("tmp/foo")` -> cached_perm=ASK -> ask daemon -> **sleeps until decision**

## The Ask Protocol

When a thread accesses a file whose effective permission is `ask`:

```
  Thread (kernel)                          Daemon (userspace)
  ──────────────                           ──────────────────
  1. yolo_check_perm() -> perm == ASK
  2. Allocate yolo_perm_request {
       id, path, op, pid, comm
     }
  3. Enqueue request on sb->pending_reqs
  4. wake_up(&sb->request_waitq)
  5. wait_event_interruptible(              ioctl(GET_REQUEST) blocks
       req->done,                            until request is available
       req->decision != UNDECIDED            |
     )                                      dequeue request
     ...thread sleeps...                     -> struct yolo_ctl_request { id, path, op, ... }
                                             |
                                            Daemon shows prompt / applies policy
                                             |
                                             ioctl(PUT_RESPONSE) -> struct yolo_ctl_response {
                                                         id: 42, decision: ALLOW }
                                              |
   6. req->decision = ALLOW                  ioctl handler:
   7. complete(&req->done)                     find request by id
     ...thread wakes...                       set decision
  8. Proceed with operation                  complete(&req->done)
     (one-time; daemon may separately
      `ioctl(RULE_ADD)` to persist)
```

Key properties:

- **Interruptible sleep**: The thread can be killed with `SIGKILL`. The
  request is removed from the pending list and `-EINTR` is returned.
- **Timeout**: Configurable via mount option `ask_timeout=<seconds>`.
  If the daemon doesn't respond, the default action (configurable:
  `deny` or `read`) is applied.
- **Minimal response**: `yolo_ctl_response` only carries `{ id, decision }`.
  Persisting policy is always a separate `ioctl(YOLO_IOC_RULE_ADD)`.
- **One-time by default**: The decision applies to this single access only.
  Next access to the same file triggers ask again. To persist a decision,
  the daemon separately calls `ioctl(YOLO_IOC_RULE_ADD)` to install a rule
  on the dentry.

## Why Dentry Walk-Up?

The rule engine must satisfy these design principles:

1. **Fast checks** — permission resolution must not scale with the number of
   rules. O(n) scanning per access is unacceptable.
2. **Hierarchical rules** — a single rule on a directory applies to all files
   underneath. Rules can overlap (e.g., `/etc` = deny, `/etc/hosts` = read)
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

- On rule add/remove: `atomic_inc(&sb->perm_gen)`. All inode caches go
  stale; next `permission()` call re-resolves lazily via `d_find_alias()` +
  walk up. O(1) invalidation.
- On `YOLO_IOC_JUMP` (after userspace commit/abort/jump): bumps perm_gen
  and shrinks the dentry cache, so permission re-resolution picks up changes.
- On `rename`: pure renames do **not** bump `perm_gen`. The inode keeps its
  `cached_perm` until some later invalidation event (rule add/remove or
  `YOLO_IOC_JUMP`). This is intentional: rename is treated as a path
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

Whenever a gate returns `-EACCES`, the kernel appends a `B\0<path>\n`
record to the journal (the *target* path the agent tried to act on, not
the parent whose perm was the source of denial). `yolo audit` surfaces
these so the user can review what was blocked, in order, relative to
snapshots. `HIDE` paths return `-ENOENT` and are not logged. See
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
"/usr"           = "read"
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
If `/foo` has no rule and `/foo/bar` has `READ`, then `/foo/bar` is
readable but `/foo/baz` is denied. However, you **cannot** deny a child
when a parent is allowed: if `/foo` grants `READ`, then `/foo/bar` also
gets `READ` and there is no way to revoke it. YoloFS uses nearest-ancestor
wins: `/foo = allow` + `/foo/bar = deny` works because the walk-up
finds `/foo/bar`'s rule first. Both directions (allow parent deny child,
deny parent allow child) are supported.

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

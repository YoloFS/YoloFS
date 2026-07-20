# Permission Gating Layer

The permission gating layer controls which files an agent can access and how.
Absent any matching rule, a path resolves to the built-in default `ask` state
(a dentry with no rule is `UNSET` and inherits from its nearest ancestor; the
root falls back to `ask`). A rule engine promotes matching paths to `allow`,
`write-ask`, `read-only`, `ask`, or `deny` — and the shipped default config
(`user/templates/yolofs.toml`) installs such rules out of the box (e.g.
`/usr` read-only, `/etc` write-ask, `.` allow), so most paths resolve to those
rather than `ask` in practice. When an access needs approval (`ask`, or a write
under `write-ask`), the thread is put to sleep; a userspace daemon receives the
request and writes back a decision that wakes the thread.

Permission gating applies to **file access** (open for read/write/exec) and
**metadata mutations** (create, mkdir, unlink, rmdir, rename, symlink).
Directory mutations use the permission of the parent directory. Directory
read-like operations (lookup/traversal, stat) are **not** gated. The one
exception is **listing**: `deny` on a directory blocks enumerating its
contents (see below); `ask` and the other policies do not affect read-like ops.

## Policy States

`enum yolo_perm` is the **policy** enum — the mutually-exclusive rule a user
sets on a path. The resolved *access* value a check computes (by walking up to
the nearest ancestor rule) draws from the same enum.

```c
enum yolo_perm {
    YOLO_PERM_UNSET,       // No rule on this dentry (walk up to find one).
    YOLO_PERM_ASK,         // Default. Block thread, ask userspace.
    YOLO_PERM_ALLOW,       // Read + write + execute allowed.
    YOLO_PERM_WRITE_ASK,   // Read + execute; writes ask userspace.
    YOLO_PERM_READ_ONLY,   // Read + execute; writes denied.
    YOLO_PERM_DENY,        // Files: all access denied.
                           // Dirs: traversal/stat still work, mutations denied,
                           //       and listing (getdents) is blocked.
};
```

Operations passed in ask requests:

```c
enum yolo_op {
    YOLO_OP_READ  = 1,    // File opened for reading.
    YOLO_OP_WRITE = 2,    // File opened for writing (includes append/truncate).
};
```

## Two Concerns: Policy and Access

`yolofs` separates the rule a user sets from the value a check resolves:

- **Policy** — the rule a user sets on a path (`allow`/`write-ask`/`read-only`/
  `ask`/`deny`), one mutually-exclusive choice per dentry. Stored in
  `yolo_dentry_info.policy` (an `enum yolo_perm`, `UNSET` = no rule here).
- **Access** — the *resolved* decision for a path, computed on demand by walking
  up from the dentry to the nearest ancestor policy. There is no cache: each
  gated operation walks live. This gates open/mutate/setattr, and (for `deny`)
  directory listing.

## In-Kernel State

### Rule Storage — per-dentry

Rules live on the dentry; there is nothing else to store. Resolution walks the
`d_parent` chain live at check time (no cache — see [Why Walk Live](#why-walk-live)).

**Per-dentry** (`yolo_dentry_info`) — one per cached dentry:

| Field | Purpose |
|-------|---------|
| `policy` | `YOLO_PERM_UNSET` unless this dentry has an explicit rule. Set or cleared by `YOLO_IOC_RULE_SET` (a policy of `UNSET` clears it). The root dentry also starts `UNSET`; reaching the root without a rule means the built-in default `ask`. Pinned (via `dget`) while a rule is attached. |

### Per-Superblock (`yolo_sb_info`)

| Field | Purpose |
|-------|---------|
| `perm.enabled` | Bool — whether permission gating is enabled. When false, all checks are skipped. |

### Ask Protocol Engine

The ask protocol handles operations whose effective policy asks for that
operation: reads or writes under `ask`, and writes under `write-ask`. The
thread sleeps until a userspace daemon decides. Ask state lives in
`struct yolo_permission` (embedded in `yolo_sb_info` as `perm`), plus one
`struct yolo_ask` per in-flight ask.

**Ask state** (`yolo_permission`) — embedded in `yolo_sb_info`:

| Field | Purpose |
|-------|---------|
| `pending_reqs` | FIFO of `yolo_ask` structs awaiting a decision. `ASK_PEEK` reads the head without removing it; an ask is unlinked only when resolved (answered via `ASK_DECIDE`, or timed out) |
| `pending_lock` | Spinlock protecting `pending_reqs` |
| `request_waitq` | Wait queue — a daemon's blocking `ASK_PEEK` ioctl waits here for a non-empty queue |
| `next_req_id` | Atomic counter for unique request IDs |
| `timeout_ms` | Milliseconds to wait for an answer before denying (0 = infinite) |

There is **no daemon registration**. A watcher just opens the mount root (a
directory fd) and loops `ASK_PEEK` (blocking) → decide → `ASK_DECIDE`.
`ASK_PEEK` is non-consuming — it returns the head ask but leaves it queued —
and `ASK_DECIDE` resolves and removes an ask *by id*. Because matching is by
id, multiple watchers are harmless (the first `ASK_DECIDE` wins; a late or
duplicate one returns `-ENOENT`), and no exclusivity check is needed.

An ask that no watcher answers is denied once its `timeout_ms` elapses (the
requester itself unlinks and denies it). With no watcher connected, an ask
therefore waits up to `timeout_ms` before denying rather than failing
instantly; `timeout_ms = 0` (wait forever) blocks until some watcher answers.

Control ioctls live on a directory fd in the mount (there is no separate `.ctl`
file). Operations that could defeat gating — `RULE_SET`, `ASK_PEEK`,
`ASK_DECIDE` — are refused when the caller's root is *inside* the mount (a
command run via `yolo run -- <cmd>`, whose root was pivoted onto the mount), so
nothing running inside the
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
| `decision` | Allow/deny decision set by the daemon's `ASK_DECIDE` ioctl |
| `done` | Completion — the blocked thread sleeps here |

## Rule Engine

Rules are **attached to dentries** — the `policy` field, only set on dentries
with an explicit rule (`YOLO_PERM_UNSET` otherwise), pinned so the dentry is
never evicted. There is no resolved-value cache: a check walks up the
`d_parent` chain to the nearest ancestor policy, live, each time.

**Setting a rule** (`yolo rule allow src`):

1. Write the rule to `yolofs.toml` (source of truth on disk):
  ```toml
   prompt_timeout = 30

   [rules]
   "src"        = "allow"
   "/etc"       = "write-ask"
   "/etc/hosts" = "read-only"
   "/usr/bin"   = "read-only"
   "/secret"    = "deny"
  ```

   Paths can be **absolute** (`/etc`) or **relative** to the session root:
   the directory containing `.yolofs/` (equivalently, the CWD where `yolo`
   was launched). For example, `src` resolves to
   `/home/user/project/src`.
2. If a mount exists (`.yolofs/mnt` is mounted), also apply live: the CLI
   opens the target *through the mount* (`<mnt>/<abs-path>`) with `O_PATH`
   and passes the fd in `ioctl(YOLO_IOC_RULE_SET, { fd, perm })` -> the
   kernel verifies the fd's dentry belongs to this mount, sets
   `YOLO_D(dentry)->policy`, and pins the dentry. The change is visible to the
   next access immediately (each check walks live). Passing an fd instead of a path string
   means the kernel never re-resolves the path (one resolution, at `open()`
   time), the in-mount check is exact (it tests the object the rule attaches
   to), and rule paths are not subject to `YOLO_PATH_MAX`.

If no mount exists, the rule is persisted to `yolofs.toml` only. It will be
applied on the next `yolo mount`.

**Rules require the target to exist.** This is a deliberate decision, not a
gap: a rule on a not-yet-existing path would need either off-spec dcache
topology (children under negative dentries) or a parallel pending-rule
structure, and inheritance already covers the common case — a rule on the
nearest existing ancestor applies to everything created underneath it. A
rule whose path doesn't exist stays in `yolofs.toml` (mount-time apply warns
and skips it) and takes effect once the path exists, on the next mount or
`yolo rule` invocation. To gate a future path ahead of time (e.g. `deny` a
directory the agent has not created yet), create the directory first, then
set the rule.

On mount, the CLI reads `yolofs.toml` and applies all `[rules]` via ioctl.

**Changing a rule**: just set it again — the next access resolves it.

**Removing a rule** (`yolo rule unset /foo/bar`):

1. Remove the rule from `yolofs.toml`.
2. If a mount exists, also apply live: open the target through the mount
   with `O_PATH` and send `ioctl(YOLO_IOC_RULE_SET)` with policy `UNSET` ->
   kernel sets `YOLO_D(dentry)->policy = UNSET` and unpins the dentry. Rule
   dentries are pinned in the dcache so the target stays findable to be unset.

**Access resolution — walk up the dentry chain, live, per check**:

```c
// Resolve policy by walking up the dentry chain. Returns the nearest
// ancestor's policy. Called live by every gated check, and by RULE_RESOLVE.
// @source (optional out-param) receives the dget'd rule dentry for the ask
// path; pass NULL when only the resolved policy is needed.
enum yolo_perm yolo_perm_walk(struct dentry *dentry, struct dentry **source)
{
    struct dentry *cur = dentry;
    while (cur) {
        struct yolo_dentry_info *di = YOLO_D(cur);
        if (di && di->policy != YOLO_PERM_UNSET)
            return di->policy;
        if (cur == cur->d_parent)
            break;              // reached root dentry
        cur = cur->d_parent;
    }
    return YOLO_PERM_ASK;   // built-in default; no rule path
}

// Shared access check: resolve (walk live), ask if needed, check the op.
// Used by yolo_open (file access) and yolo_check_mutate_perm (metadata ops).
// Journaling lives next to the result: an ask writes a G result internally; a
// static denial writes G with result `d`. `check` is whose access gates;
// `target` is the path recorded by G. Callers propagate the returned errno.
int yolo_perm_check_dentry(struct yolo_sb_info *sbi, struct dentry *check,
                           struct dentry *target, int f_flags)
{
    struct dentry *source = NULL;
    enum yolo_perm perm = yolo_perm_walk(check, &source);  // one walk
    // ask on ASK / WRITE_ASK+write (consumes source), else static (dputs it);
    // writes exactly one G ...
}

// Metadata ops check write access on the parent directory; a block reports
// the child (target). Exactly-one-G behavior is handled in the shared check.
static int yolo_check_mutate_perm(struct dentry *dentry)
{
    struct yolo_sb_info *sbi = YOLO_SB(dentry->d_sb);
    if (!sbi->perm.enabled)
        return 0;
    return yolo_perm_check_dentry(sbi, dentry->d_parent, dentry, O_WRONLY);
}

// yolo_permission(): regular files pass (access is gated authoritatively in
// yolo_open, which holds the exact dentry and may sleep; writes COW, so the
// lower mode is irrelevant). Directories/symlinks delegate to the lower FS for
// traversal and dir mode bits. A `deny` directory's listing is blocked in
// yolo_readdir, not here. This callback only has the inode; it never resolves
// name-based access. Cannot sleep (may run under RCU).
static int yolo_permission(struct mnt_idmap *idmap,
                           struct inode *inode, int mask)
{
    if (S_ISREG(inode->i_mode))
        return 0;
    return inode_permission(idmap, yolo_lower_inode(inode), mask);
}
```

The root dentry starts as `YOLO_PERM_UNSET`. Reaching the root without an
explicit rule returns the built-in default `YOLO_PERM_ASK`; an explicit
`/ = "ask"` rule is a real root-dentry rule reported to userspace as
`rule_path = "/"`. A check is an O(depth) walk up `d_parent` to the nearest
rule — a few pointer hops over dentries the pathwalk just touched, once per
gated operation (not per byte, not per path component). A rule change is
visible to the next access with no invalidation step, because the walk always
reads the current `policy`.

`yolo_open` is the single authority for regular-file access (the access
decision may sleep to `ask`):

```c
static int yolo_open(struct inode *inode, struct file *file)
{
    struct dentry *dentry = file->f_path.dentry;

    if (sbi->perm.enabled) {
        // check == target: the file's own access gates its open.
        err = yolo_perm_check_dentry(sbi, dentry, dentry, file->f_flags);
        if (err)
            return err;
    }
    // ... staging redirect (lazy COW, see staging.md#open--read--write-path) ...
}
```

`deny` on a directory blocks *listing* its contents. The block lives in
`yolo_readdir` (getdents), not `yolo_dir_open` — opening the directory fd must
still succeed so the control ioctls, which live on a mount directory fd, keep
working even under a `deny`:

```c
static int yolo_readdir(struct file *file, struct dir_context *ctx)
{
    if (sbi->perm.enabled &&
        yolo_perm_walk(file->f_path.dentry, NULL) == YOLO_PERM_DENY)
        return -EACCES;
    // ... merge staged + base entries ...
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
 yolo rule deny  ~/.mozilla
```

- `open("src/main.rs")` -> walk resolves ALLOW -> **pass**
- `open("etc/passwd", O_RDONLY)` -> resolves WRITE_ASK -> **pass** (read/exec allowed)
- `open("etc/passwd", O_WRONLY)` -> resolves WRITE_ASK -> **ask daemon on write**
- `open("etc/hosts", O_WRONLY)` -> resolves READ_ONLY -> **deny write**
- `readdir("etc")` -> **pass** (only `deny` blocks listing)
- `stat("etc")` -> **pass** (dir read-like ops not gated)
- `open("tmp/foo")` -> resolves ASK -> ask daemon -> **sleeps until decision**
- `readdir("mozilla")` under `deny` -> **EACCES** (listing blocked); `open("mozilla/x")` -> **EACCES**

## The Ask Protocol

When a thread accesses a file whose effective permission is `ask`, or writes a
file whose effective permission is `write-ask`:

```
  Thread (kernel)                          Daemon (userspace)
  ──────────────                           ──────────────────
  1. yolo_perm_check_dentry() -> perm asks for this op
  2. Allocate yolo_ask {
       id, access_path, rule_path, rule_perm, op, pid, comm
     }
  3. Enqueue request on sb->pending_reqs
  4. wake_up(&sb->request_waitq)
  5. wait_event_interruptible(              ioctl(ASK_PEEK) blocks
       req->done                             until a request is queued
       (completion)                          |
     )                                      read head request (not removed)
     ...thread sleeps...                     -> struct yolo_ioc_ask {
                                                       id, access_path,
                                                       rule_path, rule_perm, op, ...
                                                    }
                                             |
                                            Daemon shows prompt / decides
                                             |
                                             ioctl(ASK_DECIDE) -> struct yolo_ioc_decision {
                                                         id: 42, decision: ALLOW }
                                              |
   6. req->decision = ALLOW                  ioctl handler (under pending_lock):
   7. complete(&req->done)                     find request by id, set decision,
     ...thread wakes...                        unlink it, complete(&req->done)
  8. Proceed/fail this operation
```

Key properties:

- **Interruptible sleep**: The thread can be killed with `SIGKILL`. The
  request is removed from the pending list and `-EINTR` is returned.
- **Timeout**: Configured in `yolofs.toml` as `prompt_timeout` (seconds,
  fractional allowed) and passed to the kernel as the `prompt_timeout_ms`
  mount option. If the daemon doesn't respond in time, the request is denied.
- **Minimal response**: `yolo_ioc_decision` only carries `{ id, decision }`.
  Valid decisions are always `allow` or `deny`; they answer only the current
  blocked operation.
  Persisting policy is always a separate `ioctl(YOLO_IOC_RULE_SET)`.
- **One-shot decisions**: Ask decisions do not mutate the cached rule mode. An
  `allow` decision lets only the current access proceed; a later access asks
  again unless userspace separately calls `ioctl(YOLO_IOC_RULE_SET)` to install
  a persistent rule. `write-ask` likewise keeps asking on later writes.

## Why Walk Live?

The rule engine must satisfy these design principles:

1. **Fast checks** — permission resolution must not scale with the number of
   rules. O(n) scanning per access is unacceptable.
2. **Hierarchical rules** — a single rule on a directory applies to all files
   underneath. Rules can overlap (e.g., `/etc` = write-ask, `/etc/hosts` = read-only)
   and the most specific path always wins, regardless of insertion order.
3. **Dynamic rules** — adding, changing, or removing a rule must take effect
   immediately without expensive invalidation.

These principles rule out most alternatives:

| Approach | Violates |
|---|---|
| Sorted array scan | #1 — O(n) per access |
| First-match glob list | #1 — O(n), #2 — order-dependent |
| Per-file hashtable | #2 — no subtree support without enumerating all files |

The **dentry tree is already a path-component trie**. Walking `d_parent` from
the target to the nearest rule is longest-prefix-match for free, and it's done
**live on each gated check** — no cache, no generation counter. This satisfies
all three principles:

1. O(depth) — typically 3-8 pointer hops, independent of rule count, over
   dentries the pathwalk just made hot; and it runs once per gated operation
   (open/mutate/setattr/getdents), never per path component or per byte.
2. The walk finds the nearest ancestor with a rule — subtrees, overlaps, and
   per-file overrides all fall out naturally from bottom-up traversal.
3. Just set `dentry->policy`; the next check walks and sees it. No child
   invalidation, no generation bump, immediate effect — including after a
   `rename` (the moved dentry resolves at its new path on the next access).

A resolved-value cache once lived here (a per-dentry `cached_access` +
generation counter) to make `->permission` O(1) on the per-path-component
traversal path. Once `->permission` stopped resolving policy (it delegates for
dirs, passes for regular files), the only remaining resolutions became
per-operation, where the O(depth) walk is negligible — so the cache and its
invalidation machinery were removed. See `docs/plans/77-drop-access-cache-walk-live.md`.

**What is gated**:

| Operation | Check | Gate point |
|-----------|-------|------------|
| open (read/write/exec) | file's own access | `yolo_open` → `yolo_perm_check_dentry` |
| readdir (listing) | `deny` blocks it | `yolo_readdir` (`deny` → EACCES) |
| stat | not gated | `yolo_getattr` (delegate) |
| lookup / traversal | not gated | `yolo_permission` (delegate for dirs) |
| create, mkdir, symlink | parent dir's access (write) | `yolo_check_mutate_perm` |
| unlink, rmdir | parent dir's access (write) | `yolo_check_mutate_perm` |
| rename | both parents' access (write) | `yolo_check_mutate_perm` × 2 |

File access (`allow`/`deny`/`read-only`/`ask`/`write-ask`) is enforced **only**
at the operation gates above (`open`, mutate, `setattr`), each of which holds
the exact dentry. `yolo_permission` does **not** decide regular-file access:
a regular file passes there unconditionally (a write is COW'd into staging, so
the lower file's unix mode is irrelevant; a read still opens the lower file,
whose mode the lower FS enforces at open). A consequence is that
**`access(2)`/`faccessat(2)` on a regular file does not reflect the yolo access
policy** (it reports success) — the real gate is `open`. The one directory
read-like op that *is* gated is **listing**: `deny` on a directory makes
`yolo_readdir` return EACCES (enumeration blocked), while traversal to
explicitly-allowed children still works.

The kernel appends one `G\0<path>\0<op>\0<result>\n` record for each prompted
or denied access. `<path>` is the target the agent tried to access, including
the child of a parent-gated metadata mutation, and `op` is `r` or `w`. The
result is `d` for a static-policy denial, `y` for an ask that allows, and `n`
for an ask that denies, including timeout denial. Direct static allows are not
logged. `yolo review` and `yolo journal` surface G records in order relative to
snapshots.

A successful `yolo rule` assignment on a live mount appends
`C\0<path>\0<policy>\n`. C stands for Configure. The one-letter policy is `q`
for ask, `a` for allow, `w` for write-ask, `r` for read-only, `d` for deny, or
`u` for unset. Applying saved rules during mount or
remount does not emit C. C records are chronological audit events: travel does
not restore policy or make an earlier C unreachable. Neither G nor C affects
staged state or the dirty bit. Review retains repeated G and C records in
journal order. See
[staging.md §Journal Format](staging.md#journal-format) for the record
shape and semantics.

**What is NOT gated**:

| Operation | Reason |
|-----------|--------|
| readlink | Symlink target; no open |
| stat / lookup | Metadata read-like ops; delegate to lower |

### Deny

Under `deny` on a directory, an agent **cannot** read/modify files under it,
create/delete entries, or **list** the directory's contents (`readdir` →
EACCES). It *can* still traverse to a child that a more specific rule
explicitly allows (nearest-ancestor wins), and `stat` on a known path still
succeeds.

**What deny does not do:** it does not hide *existence*. The denied
directory's own name is still visible in its parent's listing; `stat` of the
directory succeeds; and probing a *guessed* path distinguishes existence via
the error code (`EACCES` vs `ENOENT`). Content is protected; existence is not.
Blocking listing does stop *bulk* enumeration (`ls`, `find`, tree walks) of the
denied subtree, which is the common leak. (`yolofs` has no separate
existence-hiding policy; the formal model in `paper/figures/model.tex` lists
exactly `ask | allow | write-ask | read-only | deny`.)

In `yolofs.toml`:

```toml
[rules]
"/usr"           = "read-only"
"/home/user/src" = "allow"
"/home/user/tax2025"          = "deny"
"/home/user/.mozilla"         = "deny"
```

## Comparison with Landlock

Landlock is a Linux Security Module (LSM) for unprivileged process
sandboxing. It shares the goal of path-based access control but differs
significantly in design.

**Rule interface**: both use file descriptors to identify rule targets — the
userspace process opens the path with `O_PATH` and passes the fd
(`landlock_add_rule()` / `YOLO_IOC_RULE_SET`), so registration is a single
resolution with no kernel-side re-walk. They diverge in what the rule
attaches to: Landlock resolves the fd to an *inode*, so rules follow the
object across renames; YoloFS takes the fd's *dentry*, so rules are
name-based — a file renamed away from a ruled path falls back to its new
location's inherited rule.

**Rule storage**: Landlock stores rules in an rb-tree keyed by inode object
pointer, one tree per ruleset. On access, it walks up every ancestor of the
target path and does an rb-tree lookup for each — O(depth x log n). YoloFS
stores rules directly on dentries and resolves by walking `d_parent` to the
nearest rule — O(depth) with no per-ancestor tree lookup, no cache.

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
YoloFS rules can be added, changed, or removed at any time via ioctl; the next
access sees the change (resolution walks live, so there is nothing to
invalidate).

**Default policy**: Landlock is deny-by-default for "handled" access rights.
YoloFS is ask-by-default — unmatched paths trigger the ask protocol, which
blocks the thread until a daemon decides.

**Scope**: Landlock is per-process (attached to credentials, inherited by
children). YoloFS is per-mount (all processes inside the mount share the same
rules and staging area).

| Aspect | Landlock | YoloFS |
|---|---|---|
| Rule target | fd -> inode (follows renames) | fd -> dentry (name-based) |
| Rule storage | rb-tree per ruleset | `policy` field on dentry |
| Access check | O(depth x log n) per ancestor | O(depth) live walk-up, no cache |
| Overlap support | Additive only (can't deny child of allowed parent) | Nearest-ancestor wins (both directions) |
| Dynamic rules | No (immutable after enforce) | Yes (add/remove/change anytime) |
| Default | Deny (handled rights) | Ask (block + prompt) |
| Scope | Per-process (cred-attached) | Per-mount |
| Staging | N/A | Full commit/abort staging layer |

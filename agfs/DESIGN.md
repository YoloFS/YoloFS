# agfs — Agentic Filesystem

A Linux kernel stackable filesystem that provides **staging-commit semantics**
and **progressive permission gating** for AI-agent sandboxing.

---

## 1. Overview

agfs stacks on top of any lower filesystem (ext4, xfs, NFS, …) using VFS
interposition (the wrapfs pattern). It adds two orthogonal capabilities:


| Capability            | Summary                                                                                                                                                                                                                                                                        |
| --------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| **Staging-commit**    | Every write goes to a staging layer. Changes are invisible to the lower FS until an explicit `commit`. An `abort` discards them instantly. |
| **Permission gating** | Every file starts in the `ask` state. A rule engine promotes matching paths to `allow-rw`, `allow-ro`, or `deny`. When a thread touches an `ask` file, the thread is put to sleep; a userspace daemon receives the request and writes back a decision that wakes the thread. |


Design goals:

- **In-kernel, zero-copy data path** — no FUSE overhead, no context switches
for allowed operations.
- **Unprivileged mounting** via user namespaces (same as current agfs).
- **Composable** — staging and permission gating are independent layers;
either can be disabled at mount time.

---

## 2. Architecture

```
 ┌──────────────────────────────────────────────────┐
 │                   User Process                    │
 │              (AI agent / shell / …)               │
 └────────────────────┬─────────────────────────────┘
                      │ VFS syscall
 ┌────────────────────▼─────────────────────────────┐
 │                    agfs                           │
 │  ┌─────────────┐  ┌──────────────┐  ┌─────────┐ │
 │  │ Perm Gating │→ │   Staging    │→ │ Passthru│ │
 │  │   Layer     │  │    Layer     │  │  Layer  │ │
 │  └─────────────┘  └──────────────┘  └─────────┘ │
 └────────────────────┬─────────────────────────────┘
                      │ vfs_*() on lower FS
 ┌────────────────────▼─────────────────────────────┐
 │              Lower filesystem (ext4 …)            │
 └──────────────────────────────────────────────────┘

 ┌──────────────────────────────────────────────────┐
 │  ./agfs/ctl  (virtual control file)     │
 │    ← read():  dequeue pending perm request       │
 │    → write(): post decision for a request id     │
 │    ← ioctl(): rules / cache invalidation         │
 │    ← poll():  POLLIN when requests are pending   │
 └──────────────────────────────────────────────────┘
```

The three internal layers execute in order for every VFS operation:

1. **Perm Gating Layer** — resolves the effective permission for the file.
   Only applies to **regular files** — directories always pass through
   (controlled by standard Unix permissions on the lower FS).
   If `ask`, sleeps the calling thread. If `deny`, returns `-EACCES`.
   If `allow-*`, falls through.
2. **Staging Layer** — routes reads to the staging dir if the file has been
   modified, otherwise to the base. Ensures writes go to the staging directory.
   Handles whiteouts for deletions.
3. **Passthrough Layer** — delegates the actual I/O to the lower filesystem
   (the wrapfs delegation pattern: swap the `kiocb->ki_filp` / call
   `lower_inode->i_op->…`).

---

## 3. Staging-Commit Mechanism

### 3.1 Concepts


| Term                  | Meaning |
| --------------------- | ------- |
| **base**              | Always `/` — the entire root filesystem, read-only from agfs's perspective until commit. |
| **staging directory** | `./agfs/staging/` — stores modified files, mirroring the base tree structure. Each staging file is always a **complete copy** — no partial/block-level tracking. |
| **whiteout**          | A character device node with major/minor 0/0 in the staging directory, indicating that the corresponding base file has been deleted. Same convention as overlayfs. |
| **mount point**       | `./agfs/mnt/` — the agent's view of the filesystem. Shows the merged base + staging with permission gating applied. |
| **commit**            | Applies all staged files and whiteouts to the base filesystem. |
| **abort**             | Discards the staging directory. O(1). |


### 3.2 Storage Layout

```
./agfs/                          # created by `agfs` in CWD
├── ctl                          # virtual control file (read/write/poll/ioctl)
├── log                          # virtual log file (read/poll) for debugging
├── config.toml                  # TOML: mount options + rules
├── renames                      # rename records: src\0dst\0 pairs (for commit)
├── staging/                     # staged files + whiteouts (mirrors / tree)
└── mnt/                         # mount point — agent works here
```

### 3.3 Path Resolution

```
resolve(relpath):
    if staging_has(relpath):
        if is_whiteout(staging_path(relpath)):  return -ENOENT
        return staging_path(relpath)
    if base_has(relpath):        return base_path(relpath)
    return -ENOENT
```

A whiteout is a char device with major/minor 0/0 (`mknod(path, S_IFCHR, 0)`).
Staging files take priority over base. Whiteouts in the staging dir shadow the base.

### 3.4 Open / Read / Write Path

Staging redirection is decided at `open()` time based on the flags:

```
agfs_open(inode, file):
    if file->f_flags & O_TRUNC:
        // Truncating write (echo >, cat >, editors): create empty staging file
        // directly. No need to copy the base file.
        file_info->lower_file = create_and_open(staging_path, file->f_flags)
        file_info->needs_cow = false

    elif file->f_flags & (O_WRONLY | O_RDWR):
        if staging_has(inode):
            // Already in staging from a prior write.
            file_info->lower_file = open(staging_path, file->f_flags)
            file_info->needs_cow = false
        else:
            // In-place write without truncate. Open base read-only for
            // now; first actual write will copy base → staging.
            file_info->lower_file = open(base_path, O_RDONLY)
            file_info->needs_cow = true
    else:
        // Read-only open: prefer staging, fall back to base.
        file_info->lower_file = open(resolve(inode), O_RDONLY)
        file_info->needs_cow = false

agfs_read_iter(kiocb, iov_iter):
    lower_file = file_info->lower_file
    kiocb->ki_filp = lower_file
    ret = lower_file->f_op->read_iter(kiocb, iov_iter)
    kiocb->ki_filp = file   // restore
    return ret

agfs_write_iter(kiocb, iov_iter):
    if file_info->needs_cow:
        // First write on a non-truncating open: copy base → staging.
        vfs_copy_file_range(base_file, staging_file, ...)
        fput(file_info->lower_file)
        file_info->lower_file = open(staging_path, file->f_flags)
        file_info->needs_cow = false

    lower_file = file_info->lower_file
    kiocb->ki_filp = lower_file
    ret = lower_file->f_op->write_iter(kiocb, iov_iter)
    kiocb->ki_filp = file   // restore
    fsstack_copy_inode_size(inode, file_inode(lower_file))
    return ret
```

**Three cases, in order of likelihood for agent workloads:**

1. `O_TRUNC` (most common: `>`, editors, code generators) → zero-copy,
  create empty staging file directly.
2. `O_RDONLY` → no staging file involvement unless file was previously written.
3. `O_RDWR`/`O_WRONLY` without truncate (rare: `sed -i`, `dd`, append) →
  copy base→staging on first write.

**Why copy the whole file for case 3 (not partial/block-level)?**

- agfs is a VFS shim — it doesn't have access to block-level structures.
  Tracking byte-range dirtiness would mean reimplementing a block layer
  on top of files (bitmaps, split reads, range merging).
- In-place partial writes to large files are not the target use case.
- Source files are small (KB–low MB). A full copy is sub-millisecond.
- Commit is a simple `vfs_rename()` per file — no reassembly.

### 3.5 Rename Handling

Renaming a file that only exists in staging is trivial — just `vfs_rename()`
within the staging directory. The interesting case is renaming a base file
that has not been modified.

**Problem**: a naïve approach would copy the entire base file into staging
just to move it. For large files this is wasteful — the content hasn't
changed, only the path.

**Solution**: record the rename in `./agfs/renames` and redirect the new
dentry's `lower_path` to the original base file. No data copy.

```
agfs_rename(old_dir, old_dentry, new_dir, new_dentry):
    old_info = AGFS_D(old_dentry)
    new_info = AGFS_D(new_dentry)

    if file is in staging:
        // Already staged — just rename within staging dir.
        vfs_rename(staging/old_path, staging/new_path)
    else:
        // File only in base. Redirect, don't copy.
        new_info->lower_path = old_info->lower_path   // point to base file
        append(renames_file, old_path, new_path)       // persist for commit

    // Hide the old path.
    create_whiteout(staging/old_path)
```

**Read after rename**: the new dentry's `lower_path` points to the original
base file. Reads go there directly. No extra lookup or table scan.

**Write after rename**: triggers lazy COW as usual. The base file (at the
original path) is copied into `staging/new_path`, and `lower_path` is
updated to point to the staging file. The `.renames` entry stays on disk
so commit knows to also delete the original.

**Commit**: userspace reads `./agfs/renames` and handles each entry:
- If a staging file exists at `new_path` (file was written after rename):
  copy staging file → `base/new_path`, then `unlink(base/old_path)`.
- If no staging file (never written): `rename(base/old_path, base/new_path)`.

**Abort**: delete `./agfs/renames` along with the staging directory.

### 3.6 Staging Operations (Userspace)

Commit and abort are **userspace operations** — the kernel module only handles
I/O redirection. The `agfs` CLI tool walks the staging
directory and applies changes to the base:

**Commit** (`agfs commit`):

1. Read `./agfs/renames` and process each entry:
   - If staging file exists at `new_path`: copy staging → `base/new_path`,
     then `unlink(base/old_path)`.
   - If no staging file: `rename(base/old_path, base/new_path)`.
2. Walk `./agfs/staging/` recursively.
3. For each whiteout (char dev 0/0): `unlink()` / `rmdir()` the corresponding
   base path.
4. For each regular file: `rename()` from `staging/<path>` → `base/<path>`,
   creating parent directories as needed.
5. Remove the `staging/` directory and `renames` file.
6. Signal the kernel module to invalidate caches.

**Abort** (`agfs abort`):

1. `rm -rf ./agfs/staging/` and `rm ./agfs/renames`.
2. Signal the kernel module to invalidate caches.

**Status** (`agfs status`):

1. Walk `staging/`, classify each entry as added (no base), modified (base
  exists), or deleted (whiteout). Print summary.

**Diff** (`agfs diff`):

1. Walk `staging/` recursively.
2. For each regular file: run unified diff against the corresponding base file.
   Added files are shown as entirely new. Modified files show the delta.
3. For each whiteout: show as a deleted file.
4. Output in standard unified diff format (compatible with `patch`).

---

## 4. Progressive Permission Mechanism

### 4.1 Permission States

```c
enum agfs_perm {
    AGFS_PERM_NONE,        // No rule on this dentry (walk up to find one).
    AGFS_PERM_ASK,         // Default. Block thread, ask userspace.
    AGFS_PERM_ALLOW_RW,    // Read + write + create + delete allowed.
    AGFS_PERM_ALLOW_RO,    // Read-only. Writes return -EACCES.
    AGFS_PERM_ALLOW_RX,    // Read + execute (for binaries/scripts).
    AGFS_PERM_DENY,        // All access returns -EACCES.
};
```

### 4.2 Rule Engine

Rules are **attached directly to dentries**. Only dentries with explicit
rules have `perm` set; all others have `AGFS_PERM_NONE`. At access time,
the kernel walks up the dentry parent chain to find the nearest ancestor
with a rule. **No caching on children, no inheritance at lookup, no
invalidation on rule change.**

**Setting a rule** (`agfs rule add /src allow-rw`):

1. Write the rule to `./agfs/config.toml` (source of truth on disk):
  ```toml
   [mount]
   ask_timeout = 30
   ask_default = "deny"

   [rules]
   "src"        = "allow-rw"
   "/etc"       = "deny"
   "/etc/hosts" = "allow-ro"
   "/usr/bin"   = "allow-rx"
  ```

   Paths can be **absolute** (`/etc`) or **relative** to the CWD where
   `agfs` was launched (`src` → resolved to `/home/user/project/src` at
   mount time).
2. `ioctl(AGFS_IOC_RULE_ADD)` → kernel resolves `/src` to a dentry.
3. Set `AGFS_D(dentry)->perm = ALLOW_RW`.
4. Pin the dentry (bump refcount so it's never evicted by memory pressure).

On mount, the kernel reads `./agfs/config.toml` and applies all rules.

**Changing a rule**: just set it again. No child invalidation needed —
children don't cache anything.

- `agfs rule add /foo/bar allow-ro` → sets perm on `/foo/bar`
- Later: `agfs rule add /foo/bar ask` → changes it. Immediate effect.

**Removing a rule** (`agfs rule remove /foo/bar`):

1. Remove the rule from `./agfs/config.toml`.
2. `ioctl(AGFS_IOC_RULE_REMOVE)` → kernel sets `AGFS_D(dentry)->perm = NONE`.
3. Unpin the dentry.
4. No invalidation needed — the walk-up will skip past it and find the
  parent's rule instead.

**Permission resolution at open() — walk up to nearest rule**:

```c
enum agfs_perm agfs_resolve_perm(struct dentry *dentry)
{
    struct dentry *cur = dentry;
    while (cur) {
        enum agfs_perm p = AGFS_D(cur)->perm;
        if (p != AGFS_PERM_NONE)
            return p;
        cur = cur->d_parent;
    }
    return AGFS_PERM_ASK;  // root default
}
```

The root dentry has `perm = AGFS_PERM_ASK`. Rule dentries are pinned and
always in memory. The walk is O(depth) — typically 3-8 pointer hops.

Gating only applies to regular files. Directories are not gated — their
access is controlled by standard Unix permissions on the lower FS.
Since the VFS `->permission()` callback only receives the inode (not the
dentry), gating is performed in `agfs_open()` where the dentry is available:

```c
static int agfs_permission(struct inode *inode, int mask)
{
    struct inode *lower = AGFS_I(inode)->lower_inode;
    return inode_permission(lower, mask);
}

static int agfs_open(struct inode *inode, struct file *file)
{
    struct dentry *dentry = file->f_path.dentry;
    int err;

    if (S_ISDIR(inode->i_mode))
        goto do_open;

    // Walk up dentry chain to find nearest rule.
    enum agfs_perm perm = agfs_resolve_perm(dentry);

    if (perm == AGFS_PERM_ASK) {
        bool persist;
        char buf[PATH_MAX];
        char *relpath = dentry_path_raw(dentry, buf, PATH_MAX);
        err = agfs_ask_userspace(dentry, relpath, file->f_flags, &perm, &persist);
        if (err)
            return err;
        if (persist)
            AGFS_D(dentry)->perm = perm;
    }

    err = agfs_check_perm(perm, file->f_flags);
    if (err)
        return err;

do_open:
    // ... staging redirect (lazy COW) ...
}
```

**Example**:

```bash
agfs rule add /src         allow-rw
agfs rule add /etc         deny
agfs rule add /etc/hosts   allow-ro
agfs rule add /usr/bin     allow-rx
```

- `open("src/main.rs")` → walk: main.rs(NONE) → src(ALLOW_RW) → **allow-rw**
- `open("src/deep/nested/f.py")` → walk: f.py → nested → deep → src(ALLOW_RW) → **allow-rw**
- `open("etc/passwd")` → walk: passwd(NONE) → etc(DENY) → **deny**
- `open("etc/hosts")` → walk: hosts(ALLOW_RO) → **allow-ro**
- `open("tmp/foo")` → walk: foo(NONE) → tmp(NONE) → root(ASK) → **ask**

### 4.3 The Ask Protocol

When a thread accesses a file whose effective permission is `ask`:

```
  Thread (kernel)                          Daemon (userspace)
  ──────────────                           ──────────────────
  1. agfs_check_perm() → perm == ASK
  2. Allocate agfs_perm_request {
       id, path, op, pid, comm
     }
  3. Enqueue request on sb->pending_list
  4. wake_up(&sb->request_waitq)
  5. wait_event_interruptible(              poll() on ./agfs/ctl
       req->done,                            returns POLLIN
       req->decision != UNDECIDED            ↓
     )                                      read() → dequeue request
     …thread sleeps…                         → struct agfs_ctl_request { id, path, op, ... }
                                             ↓
                                            Daemon shows prompt / applies policy
                                             ↓
                                            write() → struct agfs_ctl_response {
                                                        id: 42, decision: ALLOW_RW,
                                                        persist: 1 }
                                             ↓
  6. req->decision = ALLOW_RW               ioctl / write handler:
  7. complete(&req->done)                     find request by id
     …thread wakes…                           set decision
  8. If persist: save to dentry->perm        complete(&req->done)
     If !persist: use for this open only
  9. Proceed with operation
```

Key properties:

- **Interruptible sleep**: The thread can be killed with `SIGKILL`. The
request is removed from the pending list and `-EINTR` is returned.
- **Timeout**: Configurable via mount option `ask_timeout=<seconds>`.
If the daemon doesn't respond, the default action (configurable:
`deny` or `allow-ro`) is applied.
- **Caching**: The daemon controls whether a decision is persistent:
  - `persist: true` (default) → stored in `agfs_dentry_info->perm`.
    Subsequent accesses skip the ask path.
  - `persist: false` → used for this one open only. Next access to the
    same file triggers ask again ("allow once").

---

## 5. Data Structures

### 5.1 Superblock Info

```c
struct agfs_sb_info {
    struct super_block     *lower_sb;
    struct path             base_path;       // always "/"
    struct path             storage_path;    // ./agfs/ directory

    // Staging
    struct path             staging_dir;       // ./agfs/staging/
    struct rw_semaphore     staging_sem;     // protects staging dir

    // Permission gating
    struct list_head        pending_reqs;    // list of agfs_perm_request
    spinlock_t              pending_lock;
    wait_queue_head_t       request_waitq;   // daemon blocks here
    atomic64_t              next_req_id;
    unsigned int            ask_timeout_s;   // seconds, 0 = infinite
    enum agfs_perm          ask_default;     // fallback on timeout

    // Control file
    struct file_operations  ctl_fops;        // ./agfs/ctl file ops
};
```

### 5.2 Inode Info

```c
struct agfs_inode_info {
    struct inode           *lower_inode;     // passthrough target
    struct inode            vfs_inode;       // embedded VFS inode
};
```

### 5.3 Control File Protocol (binary)

Fixed-size structs for the `./agfs/ctl` read/write interface. No parsing —
just `copy_to_user()` / `copy_from_user()`.

```c
// kernel → userspace: read() returns one of these
struct agfs_ctl_request {
    __u64   id;
    __u32   op;                  // AGFS_OP_READ / WRITE / EXEC
    __u32   pid;
    char    comm[16];
    char    path[PATH_MAX];
};

// userspace → kernel: write() accepts one of these
struct agfs_ctl_response {
    __u64   id;
    __u8    decision;            // enum agfs_perm value
    __u8    persist;             // 1 = save to dentry, 0 = one-time
    __u8    _pad[6];
};
```

### 5.4 Pending Permission Request (internal)

```c
struct agfs_perm_request {
    u64                     id;
    char                    path[PATH_MAX];
    unsigned int            op;
    pid_t                   pid;
    char                    comm[TASK_COMM_LEN];

    enum agfs_perm          decision;       // set by daemon
    bool                    persist;        // set by daemon
    struct completion        done;          // thread sleeps on this
    struct list_head         list;          // sb->pending_reqs
};
```

### 5.5 Dentry Info

```c
struct agfs_dentry_info {
    spinlock_t              lock;
    struct path             lower_path;      // resolved lower path (staging or base)
    enum agfs_perm          perm;            // AGFS_PERM_NONE unless this dentry has a rule
};
```

### 5.6 File Info

```c
struct agfs_file_info {
    struct file            *lower_file;     // opened lower file (base or staging)
    bool                    needs_cow;      // true until first write copies base→staging
    const struct vm_operations_struct *lower_vm_ops;
};
```

### 5.7 Concurrency

| Lock | Protects | Type |
|---|---|---|
| `sb->staging_sem` | Staging directory | `rw_semaphore` (read for path resolution, write for cache invalidation) |
| `sb->pending_lock` | Pending request queue | `spinlock` |
| `dentry_info->lock` | Lower path in dentry | `spinlock` |

**Lock ordering**: `staging_sem` → `pending_lock` → `dentry_info->lock`

---

## 6. VFS Operations Map

### 6.1 Superblock Ops


| Operation       | Behavior                                                        |
| --------------- | --------------------------------------------------------------- |
| `alloc_inode`   | Allocate `agfs_inode_info` from slab cache.                     |
| `destroy_inode` | Free inode info, drop lower inode ref.                          |
| `evict_inode`   | Clear inode, drop lower inode ref.                              |
| `statfs`        | Delegate to lower FS. Replace `f_type` with `AGFS_SUPER_MAGIC`. |
| `put_super`     | Free `agfs_sb_info`, deactivate lower super.                    |
| `show_options`  | Print mount options.                                            |


### 6.2 Inode Ops (Directory)


| Operation    | Perm check                                                   | Staging layer                                                                     | Passthrough                               |
| ------------ | ------------------------------------------------------------ | --------------------------------------------------------------------------------- | ----------------------------------------- |
| `lookup`     | —                                                            | Resolve: check staging (whiteout → ENOENT, regular → use it), then base.          | `lookup_one_len()` on resolved lower dir. |
| `create`     | — (dir perm via lower FS)                                    | Create file in staging directory.                                                 | `vfs_create()` on staging dir.            |
| `mkdir`      | — (dir perm via lower FS)                                    | Create dir in staging.                                                            | `vfs_mkdir()` on staging dir.             |
| `unlink`     | — (dir perm via lower FS)                                    | Create whiteout (char dev 0/0) in staging. Remove regular staging file if exists. | —                                         |
| `rmdir`      | — (dir perm via lower FS)                                    | Create whiteout in staging. Remove staging dir if exists.                         | —                                         |
| `rename` | — (dir perm via lower FS) | See §3.5 Rename Handling. | — |
| `symlink`    | — (dir perm via lower FS)                                    | Create symlink in staging.                                                        | `vfs_symlink()`.                          |
| `permission` | **Gating for regular files; delegate to lower FS for dirs.** | —                                                                                 | `inode_permission()` on lower inode.      |
| `setattr`    | Gated (regular files only).                                  | Copy base→staging first, then setattr on staging.                                 | `notify_change()` on lower.               |
| `getattr`    | Gated (regular files only).                                  | Stat from resolved path (staging or base).                                        | `vfs_getattr()` on lower.                 |


### 6.3 File Ops


| Operation    | Behavior                                                                                                                                                                                          |
| ------------ | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `open`       | Perm gating (via dentry). If writable and staging file exists: open staging file. If writable and no staging file: open base read-only, set `needs_cow`. If read-only: open staging file or base. |
| `read_iter`  | Swap `kiocb->ki_filp` to lower file, call `lower->read_iter()`.                                                                                                                                   |
| `write_iter` | If `needs_cow`: copy base→staging, reopen as writable. Then delegate to `lower->write_iter()`.                                                                                                    |
| `mmap`       | Delegate to lower. Save `vm_ops` for fault handling.                                                                                                                                              |
| `fsync`      | Delegate to lower (staging file if COW'd).                                                                                                                                                        |
| `release`    | `fput()` lower file. Free `agfs_file_info`.                                                                                                                                                       |
| `llseek`     | Delegate to lower.                                                                                                                                                                                |


### 6.4 Dentry Ops


| Operation      | Behavior                                                                                              |
| -------------- | ----------------------------------------------------------------------------------------------------- |
| `d_revalidate` | If lower FS has revalidate, delegate. Also check staging epoch (invalidate if commit/abort occurred). |
| `d_release`    | Free `agfs_dentry_info`.                                                                              |


---

## 7. Control File (`./agfs/ctl`)

A virtual file created by the kernel module at mount time. The daemon and
CLI tools interact with it via standard file operations.

### 7.1 File Operations


| Syscall   | Behavior                                                                                                                                                                    |
| --------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `read()`  | Dequeue the oldest pending permission request. Returns one `struct agfs_ctl_request` (fixed-size binary). Blocks if queue is empty (or returns `-EAGAIN` for `O_NONBLOCK`). |
| `write()` | Submit a decision: one `struct agfs_ctl_response` (fixed-size binary). Wakes the sleeping thread.                                                                           |
| `poll()`  | Returns `POLLIN` when there are pending requests.                                                                                                                           |
| `ioctl()` | Rule management and cache invalidation (see below).                                                                                                                         |


### 7.2 Ioctl Commands

```c
// Permission management
#define AGFS_IOC_RULE_ADD        _IOW('A', 10, struct agfs_ioc_rule)
#define AGFS_IOC_RULE_REMOVE     _IOW('A', 11, struct agfs_ioc_rule)
#define AGFS_IOC_CACHE_INVAL     _IO('A', 20)
```

`AGFS_IOC_CACHE_INVAL` is called by userspace after commit/abort to
invalidate the kernel's dentry and inode caches so the mount reflects
the new base state.

---

## 8. Log File (`./agfs/log`)

A virtual read-only file for debugging and testing. The kernel writes
structured binary log entries to a ring buffer; userspace reads them from
`./agfs/log`.

### 8.1 Log Entry

```c
struct agfs_log_entry {
    __u64   timestamp_ns;        // ktime_get_real_ns()
    __u64   req_id;              // perm request id (0 if not applicable)
    __u32   op;                  // AGFS_OP_READ / WRITE / OPEN / LOOKUP / ...
    __u32   pid;
    __u8    event;               // AGFS_LOG_* (see below)
    __u8    perm;                // enum agfs_perm (result)
    __u8    _pad[6];
    char    comm[16];            // process name (TASK_COMM_LEN)
    char    path[PATH_MAX];
};

// Event types
#define AGFS_LOG_OPEN        1   // file opened (path, perm result)
#define AGFS_LOG_ASK         2   // ask request sent to daemon
#define AGFS_LOG_DECISION    3   // daemon responded (decision, persist)
#define AGFS_LOG_DENY        4   // access denied
#define AGFS_LOG_COW         5   // base→staging copy triggered
#define AGFS_LOG_RULE        6   // rule added/changed
#define AGFS_LOG_COMMIT      7   // staging committed
#define AGFS_LOG_ABORT       8   // staging aborted
```

### 8.2 File Operations


| Syscall  | Behavior                                                                                                           |
| -------- | ------------------------------------------------------------------------------------------------------------------ |
| `read()` | Returns one or more `struct agfs_log_entry` from the ring buffer. Blocks if empty (or `-EAGAIN` for `O_NONBLOCK`). |
| `poll()` | Returns `POLLIN` when log entries are available.                                                                   |


The ring buffer has a fixed size (configurable via mount option
`log_size=<n>`, default 1024 entries). Old entries are overwritten when full.

### 8.3 Usage

```bash
# Tail the log in real-time (userspace tool decodes binary entries)
agfs log --follow

# Dump all buffered entries
agfs log --dump
```

---

## 9. CLI Interface

```bash
# Mount and drop into a shell inside the sandbox
$ agfs
   → creates ./agfs/, mounts at ./agfs/mnt, runs $SHELL inside it

# Mount and run a specific command
$ agfs -- make build
   → creates ./agfs/, mounts, runs `make build` inside ./agfs/mnt, exits

# Subcommands (operate on an existing ./agfs/ session)
$ agfs status            # show staged changes
$ agfs diff              # unified diff of staged vs base
$ agfs commit            # apply staged changes to base
$ agfs abort             # discard staged changes
$ agfs rule add /src allow-rw
$ agfs rule remove /src
$ agfs log --follow      # tail the debug log
$ agfs watch             # handle ask requests (daemon mode)
```

### 9.1 Mount Options

Configured via `./agfs/config.toml` or CLI flags:

| Option | Default | Description |
|---|---|---|
| `ask_timeout` | 0 (infinite) | Seconds before ask request times out |
| `ask_default` | `deny` | Fallback permission on timeout |
| `nogating` | false | Disable permission gating entirely |
| `nostaging` | false | Disable staging (passthrough + gating only) |
| `log_size` | 1024 | Ring buffer entries for `./agfs/log` |

Inside `./agfs/mnt`, the agent sees the full root filesystem with staging
and permission gating applied. Files under the current working directory
are typically ruled `allow-rw`; everything else defaults to `ask`.

---

## 10. Source File Layout

```
agfs/
├── DESIGN.md                  # This file
├── kmod/                      # Kernel module
│   ├── Kbuild
│   ├── Makefile
│   ├── agfs.h
│   ├── super.c
│   ├── inode.c
│   ├── file.c
│   ├── dentry.c
│   ├── lookup.c
│   ├── staging.c
│   ├── perm.c
│   ├── ctl.c
│   └── log.c
└── cli/                       # Userspace CLI tool (Rust)
    ├── Cargo.toml             # path = "main.rs"
    ├── main.rs
    ├── mount.rs
    ├── rule.rs
    ├── commit.rs
    ├── abort.rs
    ├── status.rs
    ├── diff.rs
    ├── log.rs
    ├── watch.rs
    └── ctl.rs
```

---

## 11. Lifecycle Example

```
# 1. Mount agfs and enter shell (base is always /)
$ cd /home/user/project
$ agfs
   → creates ./agfs/, mounts / → ./agfs/mnt, drops into $SHELL

# 1b. Install rules via CLI (attaches perm directly to dentries)
$ agfs rule add /src allow-rw
$ agfs rule add /etc deny
$ agfs rule add /etc/hosts allow-ro

# 2. Agent writes to a file matching an allow-rw rule
$ echo "hello" > /src/main.rs
   → kernel: agfs_lookup("src") → explicit rule → perm=ALLOW_RW
   → kernel: agfs_lookup("main.rs") → no rule (NONE)
   → kernel: agfs_open() → walk up: main.rs(NONE) → src(ALLOW_RW)
   → kernel: perm=ALLOW_RW, O_WRONLY → pass
   → kernel: agfs_write_iter() → lazy COW, delegate to staging file

# 3. Agent reads /etc/passwd (denied — /etc has deny rule)
$ cat /etc/passwd
   → kernel: agfs_lookup("etc") → explicit rule → perm=DENY
   → kernel: agfs_lookup("passwd") → no rule (NONE)
   → kernel: agfs_open("passwd") → resolve perm: passwd(NONE) → etc(DENY) → -EACCES

# 4. Agent reads /etc/hosts (explicit override → allow-ro)
$ cat /etc/hosts
   → kernel: agfs_lookup("hosts") → explicit rule → perm=ALLOW_RO
   → kernel: agfs_open() → walk up: hosts(ALLOW_RO) → pass

# 5. Agent reads /tmp/secrets (no rule anywhere → walk up reaches root → ask)
$ cat /tmp/secrets
   → kernel: agfs_lookup("tmp") → no rule (NONE)
   → kernel: agfs_lookup("secrets") → no rule (NONE)
   → kernel: agfs_open() → walk up: secrets(NONE) → tmp(NONE) → root(ASK)
   → kernel: enqueue request, thread sleeps
   → daemon: read(./agfs/ctl) → agfs_ctl_request { id:1, path:"/tmp/secrets", ... }
   → daemon: decision: allow-ro
   → daemon: write(./agfs/ctl, agfs_ctl_response { id:1, decision:ALLOW_RO, persist:1 })
   → kernel: wake thread, update perm=ALLOW_RO on dentry
   → kernel: open base/tmp/secrets read-only, proceed

# 6. Agent tries to write /etc/hosts (walk up finds ALLOW_RO)
$ echo x >> /etc/hosts
   → kernel: agfs_open() → ALLOW_RO, O_WRONLY → -EACCES

# 7. Commit all staged changes to the real filesystem (userspace)
$ agfs commit
   → userspace: walk staging/, rename files to base, unlink whiteouts
   → userspace: ioctl(AGFS_IOC_CACHE_INVAL) on ./agfs/ctl
   → kernel: invalidate dentry + inode caches
```

---

## 12. Key Design Decisions

### Why dentry walk-up instead of other rule engines?

The rule engine must satisfy these design principles:

1. **Fast checks** — permission resolution must not scale with the number of
   rules. O(n) scanning per access is unacceptable.
2. **Hierarchical rules** — a single rule on a directory applies to all files
   underneath. Rules can overlap (e.g., `/etc` = deny, `/etc/hosts` = allow-ro)
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

### Why stackable VFS instead of block-level COW?

- **Portability**: Works on any underlying filesystem (ext4, xfs, NFS, tmpfs).
- **File-level granularity**: Permission gating operates on files and
  directories, which map naturally to inodes in a stackable FS.
- **Simplicity**: No need to manage block allocation, journaling, or
  filesystem metadata. The lower FS handles all of that.

### Permission cache invalidation

- On `AGFS_IOC_CACHE_INVAL` (via ./agfs/ctl after userspace commit/abort):
  staging-related caches are invalidated.
- On `rule add/remove`: **no invalidation needed**. Children don't cache
  perms; the walk-up resolves against the updated rule immediately.
- On `rename`: no invalidation needed — walk-up uses the new parent chain.


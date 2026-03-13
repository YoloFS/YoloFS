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
 │       ┌─────────────┐    ┌──────────────┐        │
 │       │ Perm Gating │ →  │   Staging    │        │
 │       │   Layer     │    │    Layer     │        │
 │       └─────────────┘    └──────────────┘        │
 └────────────────────┬─────────────────────────────┘
                      │ vfs_*() on lower FS
 ┌────────────────────▼─────────────────────────────┐
 │              Lower filesystem (ext4 …)            │
 └──────────────────────────────────────────────────┘

 ┌──────────────────────────────────────────────────┐
 │  ioctl on any agfs directory fd (.agfs/mnt)      │
 │    ← AGFS_IOC_GET_REQUEST:  dequeue perm request   │
 │    → AGFS_IOC_PUT_RESPONSE: post decision           │
 │    → AGFS_IOC_RULE_ADD/REMOVE: manage rules      │
 │    → AGFS_IOC_CACHE_INVAL: invalidate caches     │
 └──────────────────────────────────────────────────┘
```

The two layers execute in order for every VFS operation:

1. **Perm Gating Layer** — resolves the effective permission for the file.
   Only applies to **regular files** — directories always pass through
   (controlled by standard Unix permissions on the lower FS).
   If `ask`, sleeps the calling thread. If `deny`, returns `-EACCES`.
   If `allow-*`, falls through.
2. **Staging Layer** — routes reads to the staging dir if the file has been
   modified, otherwise to the base. Ensures writes go to the staging directory.
   Handles whiteouts for deletions.

All I/O is ultimately delegated to the lower filesystem via the standard
wrapfs pattern (`kiocb` swapping, `vfs_*()` calls).

---

## 3. Staging-Commit Mechanism

### 3.1 Concepts


| Term                  | Meaning |
| --------------------- | ------- |
| **base**              | Always `/` — the entire root filesystem, read-only from agfs's perspective until commit. |
| **staging directory** | `.agfs/staging/` — stores modified files, mirroring the base tree structure. Each staging file is always a **complete copy** — no partial/block-level tracking. |
| **whiteout**          | A character device node with major/minor 0/0 in the staging directory, indicating that the corresponding base file has been deleted. Same convention as overlayfs. |
| **mount point**       | `.agfs/mnt/` — the agent's view of the filesystem. Shows the merged base + staging with permission gating applied. |
| **commit**            | Applies all staged files and whiteouts to the base filesystem. |
| **abort**             | Discards the staging directory. O(1). |


### 3.2 Credential Override for Staging

The staging directory is created during mount (typically by root) and is owned
by root. When non-root user processes trigger staging operations through agfs
(create, mkdir, COW, write, etc.), the VFS permission checks on the staging
directory would fail because the user lacks write permission on root-owned
directories.

To solve this, agfs saves the mount-time credentials (`current_cred()`) in
`agfs_sb_info` during `agfs_fill_super` and uses `override_creds()` /
`revert_creds()` to temporarily assume them when performing staging directory
operations. This is the standard pattern used by overlayfs and other stackable
filesystems. The actual permission model for user access is enforced
separately by the agfs permission gating layer (§4), not by Unix mode bits on
the staging directory.

### 3.3 Storage Layout

```
agfs.toml                       # config file in CWD (mount options + rules)
.agfs/                          # created by `agfs` in CWD
├── renames                      # persisted rename log: src\0dst\0 pairs
├── staging/                     # staged files + whiteouts (mirrors / tree)
└── mnt/                         # mount point — agent works here
                                 #   ioctl on this directory fd for control
```

### 3.4 Path Resolution

```
resolve(dentry):
    relpath = dentry_relpath(dentry)

    if staging_has(relpath):
        if is_whiteout(staging_path(relpath)):  return -ENOENT
        return staging_path(relpath)

    if AGFS_D(dentry)->lower_path is set:
        return AGFS_D(dentry)->lower_path

    if base_has(relpath):
        return base_path(relpath)

    return -ENOENT
```

A whiteout is a char device with major/minor 0/0 (`mknod(path, S_IFCHR, 0)`).
Staging files take priority over base. Whiteouts in the staging dir shadow the base.
`.agfs/renames` is not consulted on the hot path. Runtime rename state lives
in dentries: a renamed destination dentry has `lower_path` redirected to the
original base object, and the old path is hidden by a staging whiteout. On
mount, agfs replays `.agfs/renames` to reinstall those redirected destination
dentries; source hiding continues to come from the whiteouts already present in
`.agfs/staging/`.

### 3.5 Open / Read / Write Path

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
        file_info->lower_file = open(resolve(file->f_path.dentry), O_RDONLY)
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

agfs_mmap(file, vma):
    if file_info->needs_cow and (vma->vm_flags & (VM_WRITE | VM_SHARED)):
        // Writable shared mapping on a file opened O_RDWR whose lower
        // file is still read-only (COW not yet triggered).  Perform COW
        // now so the lower file is writable before we delegate mmap.
        do_cow(...)
        fput(file_info->lower_file)
        file_info->lower_file = open(staging_path, file->f_flags)
        file_info->needs_cow = false

    lower_file = file_info->lower_file
    vma->vm_file = lower_file
    ret = lower_file->f_op->mmap(lower_file, vma)
    if ret:
        vma->vm_file = file   // restore on error
    else:
        fput(file)             // balance do_mmap's get_file
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

### 3.6 Create / Mkdir / Symlink Path

All inode creation operations go to the staging directory, never to the base
filesystem. This ensures that `agfs abort` can discard every change cleanly.

```
agfs_create(dir, dentry, mode):
    relpath = dentry_relpath(dentry)
    agfs_create_staging_parents(sbi, relpath)
    vfs_create() on staging dir
    interpose new inode pointing at the staging dentry

agfs_mkdir(dir, dentry, mode):
    relpath = dentry_relpath(dentry)
    agfs_create_staging_parents(sbi, relpath)
    vfs_mkdir() on staging dir
    interpose new inode pointing at the staging dentry

agfs_symlink(dir, dentry, symname):
    relpath = dentry_relpath(dentry)
    agfs_create_staging_parents(sbi, relpath)
    vfs_symlink() on staging dir
    interpose new inode pointing at the staging dentry
```

`touch` (create + close, no write) produces an empty file in staging —
visible in `agfs status` / `agfs diff` and cleanly discarded by abort.

### 3.7 Rename Handling

Renaming a file that only exists in staging is trivial — just `vfs_rename()`
within the staging directory. The interesting case is renaming a base file
that has not been modified.

**Problem**: a naïve approach would copy the entire base file into staging
just to move it. For large files this is wasteful — the content hasn't
changed, only the path.

**Solution**: append the rename to `.agfs/renames`, redirect the destination
dentry's `lower_path` to the original base object, pin that dentry until
commit/abort, and create a whiteout at the old path. The on-disk file is only
persisted recovery/commit data; kernel path resolution uses dentry state. No
data copy is needed for a pure rename.

`.agfs/renames` is a sequence of `old_path\0new_path\0` pairs. Each path is
absolute and NUL-terminated.

On mount, agfs replays this file and reinstalls the redirected destination
dentries. Each runtime rename appends one record to the file.

Each record means:

- hide `old_path` from the merged view, and
- resolve `new_path` to the base file currently stored at `old_path`
  until a staged copy exists at `new_path`.

```
agfs_rename(old_dir, old_dentry, new_dir, new_dentry):
    old_info = AGFS_D(old_dentry)
    new_info = AGFS_D(new_dentry)

    if file is in staging:
        // Already staged — just rename within staging dir.
        vfs_rename(staging/old_path, staging/new_path)
    else:
        // File only in base. Record the move; no copy yet.
        new_info->lower_path = old_info->lower_path
        dget(new_dentry)   // pin until commit/abort
        append(renames_file, old_path, new_path)

    // Hide the old path.
    create_whiteout(staging/old_path)
```

**Read after rename**: `resolve(new_dentry)` follows
`AGFS_D(new_dentry)->lower_path` and returns `base/old_path` until
`staging/new_path` exists. A lookup of `old_path` sees the whiteout and returns
`-ENOENT`.

**Write after rename**: triggers lazy COW as usual. The base file at
`old_path` is copied into `staging/new_path`; from that point on,
`resolve(new_dentry)` returns the staged file because staging wins over the
redirected `lower_path`. The rename record stays on disk so commit knows to delete the
original path after installing the new file.

Commit and abort handling for renames is covered in §3.8.

### 3.8 Staging Operations (Userspace)

Commit and abort are **userspace operations** — the kernel module only handles
I/O redirection. The `agfs` CLI tool walks the staging
directory and applies changes to the base.

All staging operations except abort share a common **staging walk**:

1. Read `.agfs/renames` and build `old→new` / `new→old` lookup tables.
2. Process rename records: for each entry, check whether a staged file
   exists at `new_path` (rename + modification) or not (pure rename).
3. Walk `.agfs/staging/` recursively.
4. Skip entries already explained by rename records:
   - whiteouts at `old_path` for renamed files,
   - staged files at `new_path` already consumed by step 2.
5. Classify remaining entries (regular files, whiteouts) per operation.

**Commit** (`agfs commit`):

1. Staging walk (above). In step 2, apply renames:
   - Staged file at `new_path`: `rename(staging/new_path, base/new_path)`,
     then `unlink(base/old_path)`.
   - No staged file: `rename(base/old_path, base/new_path)`.
2. For each remaining whiteout (char dev 0/0): `unlink()` / `rmdir()` the
   corresponding base path.
3. For each remaining regular file: `rename()` from `staging/<path>` →
   `base/<path>`, creating parent directories as needed.
4. Remove the `staging/` directory and `renames` file.
5. Signal the kernel module to invalidate caches and drop pinned rename dentries.

**Abort** (`agfs abort`):

1. `rm -rf .agfs/staging/` and `rm .agfs/renames`.
2. Signal the kernel module to invalidate caches and drop pinned rename dentries.

**Status** (`agfs status`):

1. Staging walk (above). In step 2, collect renames:
   - No staged file at `new_path`: pure rename (`old_path -> new_path`).
   - Staged file at `new_path`: rename + modified.
2. Classify remaining entries as added, modified, or deleted.

**Diff** (`agfs diff`):

1. Staging walk (above). In step 2, diff renames:
   - No staged file at `new_path`: emit rename-only record.
   - Staged file at `new_path`: diff `base/old_path` against
     `staging/new_path`, label as rename + modification.
2. For each remaining regular file: unified diff against the corresponding
   base file. Added files shown as entirely new; modified files show the delta.
3. For each remaining whiteout: show as a deleted file.
4. Output in git-style unified diff format. Pure renames are represented as
   rename metadata and are not plain POSIX `patch` input.

---

## 4. Progressive Permission Mechanism

### 4.1 Permission States

```c
enum agfs_perm {
    AGFS_PERM_NONE,        // No rule on this dentry (walk up to find one).
    AGFS_PERM_ASK,         // Default. Block thread, ask userspace.
    AGFS_PERM_ALLOW,       // Read + write + execute allowed.
    AGFS_PERM_ALLOW_RW,    // Read + write. No execute.
    AGFS_PERM_ALLOW_RO,    // Read only. No write, no execute.
    AGFS_PERM_ALLOW_RX,    // Read + execute. No write.
    AGFS_PERM_DENY,        // All access returns -EACCES.
};
```

### 4.2 Rule Engine

Rules are **attached to dentries**. Resolved permissions are **cached on
inodes** with a **generation counter** for cheap invalidation.

Two levels:
- **Dentry**: `perm` field — only set on dentries that have an explicit rule
  (`AGFS_PERM_NONE` otherwise). Rules are pinned so the dentry is never evicted.
- **Inode**: `cached_perm` + `perm_gen` — resolved permission cached during
  `lookup()` by inheriting from the nearest ancestor dentry with a rule.
  Checked in `permission()` with O(1) cost.

**Setting a rule** (`agfs rule add src allow-rw`):

1. Write the rule to `agfs.toml` (source of truth on disk):
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

   Paths can be **absolute** (`/etc`) or **relative** to the session root:
   the directory containing `.agfs/` (equivalently, the CWD where `agfs`
   was launched). For example, `src` resolves to
   `/home/user/project/src`.
2. If a mount exists (`.agfs/mnt` is mounted), also apply live:
   `ioctl(AGFS_IOC_RULE_ADD)` → kernel resolves the normalized absolute path
   to a dentry, sets `AGFS_D(dentry)->perm`, pins the dentry, and bumps
   `perm_gen` to invalidate all cached inode perms.

If no mount exists, the rule is persisted to `agfs.toml` only. It will be
applied on the next `agfs mount`.

On mount, the CLI reads `agfs.toml` and applies all `[rules]` via ioctl.

**Changing a rule**: just set it again + bump generation.

**Removing a rule** (`agfs rule remove /foo/bar`):

1. Remove the rule from `agfs.toml`.
2. If a mount exists, also apply live:
   `ioctl(AGFS_IOC_RULE_REMOVE)` → kernel sets `AGFS_D(dentry)->perm = NONE`,
   unpins the dentry, and bumps `perm_gen`.

**Permission resolution — cached on inode, resolved lazily**:

```c
// Resolve by walking up dentry chain (only called on cache miss).
enum agfs_perm agfs_resolve_perm(struct dentry *dentry)
{
    struct dentry *cur = dentry;
    while (cur) {
        if (AGFS_D(cur)->perm != AGFS_PERM_NONE)
            return AGFS_D(cur)->perm;
        cur = cur->d_parent;
    }
    return AGFS_PERM_ASK;
}

// Called during lookup() — cache the resolved perm on the inode.
void agfs_cache_perm(struct inode *inode, struct dentry *dentry)
{
    struct agfs_inode_info *info = AGFS_I(inode);
    struct agfs_sb_info *sb = AGFS_SB(inode->i_sb);

    info->cached_perm = agfs_resolve_perm(dentry);
    info->perm_gen = atomic64_read(&sb->perm_gen);
}

// Called by permission() — O(1) in steady state.
static int agfs_permission(struct inode *inode, int mask)
{
    struct agfs_inode_info *info = AGFS_I(inode);
    struct agfs_sb_info *sb = AGFS_SB(inode->i_sb);

    if (!S_ISREG(inode->i_mode))
        return inode_permission(info->lower_inode, mask);

    // Check generation — re-resolve if stale.
    enum agfs_perm perm = info->cached_perm;
    if (info->perm_gen != atomic64_read(&sb->perm_gen)) {
        struct dentry *dentry = d_find_alias(inode);
        if (dentry) {
            perm = agfs_resolve_perm(dentry);
            info->cached_perm = perm;
            info->perm_gen = atomic64_read(&sb->perm_gen);
            dput(dentry);
        }
    }

    if (perm == AGFS_PERM_ASK)
        return 0;  // ask is handled in open(), not here

    switch (perm) {
    case AGFS_PERM_ALLOW:     return 0;
    case AGFS_PERM_ALLOW_RW:
        return (mask & MAY_EXEC) ? -EACCES : 0;
    case AGFS_PERM_ALLOW_RO:
        return (mask & (MAY_WRITE | MAY_EXEC)) ? -EACCES : 0;
    case AGFS_PERM_ALLOW_RX:
        return (mask & MAY_WRITE) ? -EACCES : 0;
    case AGFS_PERM_DENY:      return -EACCES;
    default:                  return -EACCES;
    }
}
```

The root dentry has `perm = AGFS_PERM_ASK`. In steady state (no rule
changes), `permission()` is a single generation compare + switch — O(1).
On rule change, the generation bumps and inodes re-resolve lazily on
next access.

The `ask` path is handled in `agfs_open()` where the dentry is directly
available and the thread can sleep:

```c
static int agfs_open(struct inode *inode, struct file *file)
{
    struct dentry *dentry = file->f_path.dentry;
    int err;

    if (S_ISDIR(inode->i_mode))
        goto do_open;

    enum agfs_perm perm = AGFS_I(inode)->cached_perm;

    if (perm == AGFS_PERM_ASK) {
        char buf[AGFS_PATH_MAX];
        char *relpath = dentry_path_raw(dentry, buf, AGFS_PATH_MAX);
        if (IS_ERR(relpath))
            return PTR_ERR(relpath);   // -ENAMETOOLONG if path won't fit
        err = agfs_ask_userspace(dentry, relpath, file->f_flags, &perm);
        if (err)
            return err;
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
 agfs rule add src          allow-rw
 agfs rule add /etc         deny
 agfs rule add /etc/hosts   allow-ro
 agfs rule add /usr/bin     allow-rx
```

- `permission("src/main.rs")` → cached_perm=ALLOW_RW (from lookup) → **pass**
- `permission("etc/passwd")` → cached_perm=DENY → **-EACCES**
- `permission("etc/hosts")` → cached_perm=ALLOW_RO → **pass for read, deny write**
- `open("tmp/foo")` → cached_perm=ASK → ask daemon → **sleeps until decision**

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
  5. wait_event_interruptible(              ioctl(GET_REQUEST) blocks
       req->done,                            until request is available
       req->decision != UNDECIDED            ↓
     )                                      dequeue request
     …thread sleeps…                         → struct agfs_ctl_request { id, path, op, ... }
                                             ↓
                                            Daemon shows prompt / applies policy
                                             ↓
                                             ioctl(PUT_RESPONSE) → struct agfs_ctl_response {
                                                         id: 42, decision: ALLOW_RW }
                                              ↓
   6. req->decision = ALLOW_RW               ioctl handler:
   7. complete(&req->done)                     find request by id
     …thread wakes…                           set decision
  8. Proceed with operation                  complete(&req->done)
     (one-time; daemon may separately
      `ioctl(RULE_ADD)` to persist)
```

Key properties:

- **Interruptible sleep**: The thread can be killed with `SIGKILL`. The
request is removed from the pending list and `-EINTR` is returned.
- **Timeout**: Configurable via mount option `ask_timeout=<seconds>`.
If the daemon doesn't respond, the default action (configurable:
`deny` or `allow-ro`) is applied.
- **Minimal response**: `agfs_ctl_response` only carries `{ id, decision }`.
  Persisting policy is always a separate `ioctl(AGFS_IOC_RULE_ADD)`.
- **One-time by default**: The decision applies to this single access only.
  Next access to the same file triggers ask again. To persist a decision,
  the daemon separately calls `ioctl(AGFS_IOC_RULE_ADD)` to install a rule
  on the dentry.

---

## 5. Data Structures

### 5.1 Superblock Info

```c
struct agfs_sb_info {
    struct super_block     *lower_sb;
    struct path             base_path;       // always "/"
    struct path             storage_path;    // .agfs/ directory

    // Staging
    struct path             staging_dir;       // .agfs/staging/
    struct path             renames_path;      // .agfs/renames
    struct rw_semaphore     staging_sem;       // protects staging dir + rename log updates/replay
    struct list_head        pinned_dentries;   // rename-pinned dentries (agfs_pinned_dentry list)

    // Permission gating
    atomic64_t              perm_gen;        // bumped on rule change; invalidates inode caches
    struct list_head        pending_reqs;    // list of agfs_perm_request
    spinlock_t              pending_lock;
    wait_queue_head_t       request_waitq;   // daemon blocks here
    atomic64_t              next_req_id;
    unsigned int            ask_timeout_s;   // seconds, 0 = infinite
    enum agfs_perm          ask_default;     // fallback on timeout
    bool                    noperm;          // disable permission gating entirely
    bool                    nostaging;       // disable staging (passthrough + gating only)
};
```

### 5.2 Inode Info

```c
struct agfs_inode_info {
    struct inode           *lower_inode;     // passthrough target

    // Permission cache (resolved at lookup, checked at permission)
    enum agfs_perm          cached_perm;
    u64                     perm_gen;        // sb->perm_gen at time of caching

    struct inode            vfs_inode;       // embedded VFS inode
};
```

### 5.3 Control Protocol (binary, ioctl-based)

Fixed-size structs for the ioctl-based control interface on any agfs directory
fd (typically `.agfs/mnt`). No parsing — just `copy_to_user()` /
`copy_from_user()`.

```c
#define AGFS_PATH_MAX 256

// kernel → userspace: AGFS_IOC_GET_REQUEST returns one of these
struct agfs_ctl_request {
    __u64   id;
    __u32   op;                  // AGFS_OP_READ / WRITE / EXEC
    __u32   pid;
    char    comm[16];
    char    path[AGFS_PATH_MAX];
};

// userspace → kernel: AGFS_IOC_PUT_RESPONSE accepts one of these
struct agfs_ctl_response {
    __u64   id;
    __u8    decision;            // enum agfs_perm value
    __u8    _pad[7];
};
```

`path` is never truncated in-kernel. If the resolved mounted-view path does
not fit in `AGFS_PATH_MAX` bytes including the terminating NUL, the access
fails with `-ENAMETOOLONG` and no ask request is enqueued.

### 5.4 Pending Permission Request (internal)

```c
struct agfs_perm_request {
    u64                     id;
    char                    path[AGFS_PATH_MAX];
    unsigned int            op;
    pid_t                   pid;
    char                    comm[TASK_COMM_LEN];

    enum agfs_perm          decision;       // set by daemon
    struct completion        done;          // thread sleeps on this
    struct list_head         list;          // sb->pending_reqs
};
```

### 5.5 Dentry Info

```c
struct agfs_dentry_info {
    spinlock_t              lock;
    struct path             lower_path;      // resolved lower path (staging, base, or redirected base after rename)
    enum agfs_perm          perm;            // AGFS_PERM_NONE unless this dentry has a rule
};
```

For pure renamed destinations, `lower_path` points at the original base object
until the first write copies it into staging. Such destination dentries are
pinned until commit/abort so the redirect remains the runtime source of truth.

### 5.6 File Info

```c
struct agfs_file_info {
    struct file            *lower_file;     // opened lower file (base or staging)
    bool                    needs_cow;      // true until first write copies base→staging
    bool                    is_staging;     // true when lower_file points at a staging file
    const struct vm_operations_struct *lower_vm_ops;
    struct agfs_ctl_private *ctl;           // non-NULL if this fd is a ctl daemon
};
```

### 5.7 Ctl Private (per-fd daemon state)

```c
struct agfs_ctl_private {
    struct list_head        dispatched;     // requests sent to this fd
    spinlock_t              lock;
};
```

Allocated lazily on first `AGFS_IOC_GET_REQUEST`. On fd close, any
dispatched-but-unanswered requests receive the default decision.

### 5.8 Pinned Dentry (for rename tracking)

```c
struct agfs_pinned_dentry {
    struct dentry          *dentry;
    struct list_head        list;
};
```

Tracks rename-destination dentries that must remain pinned until
commit/abort/unmount so the redirected `lower_path` stays valid.

### 5.9 Concurrency

| Lock | Protects | Type |
|---|---|---|
| `sb->staging_sem` | Staging directory + `.agfs/renames` updates/replay | `rw_semaphore` (read for path resolution, write for rename/commit/abort) |
| `sb->pending_lock` | Pending request queue | `spinlock` |
| `dentry_info->lock` | Lower path in dentry | `spinlock` |

**Lock ordering**: `staging_sem` → `pending_lock` → `dentry_info->lock`
---

## 6. VFS Operations Map

### 6.1 Superblock Ops


| Operation       | Behavior                                                        |
| --------------- | --------------------------------------------------------------- |
| `alloc_inode`   | Allocate `agfs_inode_info` from slab cache.                     |
| `free_inode`    | Free inode info via slab cache.                                 |
| `evict_inode`   | Clear inode, drop lower inode ref.                              |
| `statfs`        | Delegate to lower FS. Replace `f_type` with `AGFS_SUPER_MAGIC`. |
| `put_super`     | Free `agfs_sb_info`, deactivate lower super.                    |
| `show_options`  | Print mount options.                                            |


### 6.2 Inode Ops (Directory)


| Operation    | Perm check                                                   | Staging layer                                                                     | Passthrough                               |
| ------------ | ------------------------------------------------------------ | --------------------------------------------------------------------------------- | ----------------------------------------- |
| `lookup`     | —                                                            | Resolve: check staging first; otherwise use redirected `dentry->lower_path` when present; otherwise use base (whiteout → ENOENT). | `lookup_one_len()` on resolved lower dir. |
| `create`     | — (dir perm via lower FS)                                    | Create file in staging directory.                                                 | `vfs_create()` on staging dir.            |
| `mkdir`      | — (dir perm via lower FS)                                    | Create dir in staging.                                                            | `vfs_mkdir()` on staging dir.             |
| `unlink`     | — (dir perm via lower FS)                                    | Create whiteout (char dev 0/0) in staging. Remove regular staging file if exists. | —                                         |
| `rmdir`      | — (dir perm via lower FS)                                    | Create whiteout in staging. Remove staging dir if exists.                         | —                                         |
| `rename` | — (dir perm via lower FS) | See §3.7 Rename Handling. | — |
| `symlink`    | — (dir perm via lower FS)                                    | Create symlink in staging.                                                        | `vfs_symlink()`.                          |
| `permission` | **Gating for regular files (O(1) cached); delegate to lower FS for dirs.** | —                                                                                 | `inode_permission()` on lower inode.      |
| `setattr`    | Gated (regular files only).                                  | Copy base→staging first, then setattr on staging.                                 | `notify_change()` on lower.               |
| `getattr`    | Gated (regular files only).                                  | Stat from resolved path (staging or base).                                        | `vfs_getattr()` on lower.                 |


### 6.3 File Ops


| Operation    | Behavior                                                                                                                                                                                          |
| ------------ | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `open`       | Perm gating (via dentry). If writable and staging file exists: open staging file. If writable and no staging file: open base read-only, set `needs_cow`. If read-only: open staging file or base. |
| `read_iter`  | Swap `kiocb->ki_filp` to lower file, call `lower->read_iter()`.                                                                                                                                   |
| `write_iter` | If `needs_cow`: copy base→staging, reopen as writable. Then delegate to `lower->write_iter()`.                                                                                                    |
| `mmap`       | If `needs_cow` and mapping is writable+shared: trigger COW first (same as `write_iter`), then delegate to the now-writable lower file. Otherwise delegate directly. Save `vm_ops` for fault handling. |
| `fsync`      | If `is_staging`: return 0 (staging files are ephemeral — committed or aborted, never persisted in place). Otherwise delegate to lower.                                                            |
| `release`    | `fput()` lower file. Free `agfs_file_info`.                                                                                                                                                       |
| `llseek`     | Delegate to lower.                                                                                                                                                                                |


### 6.4 Dentry Ops


| Operation      | Behavior                                                                                              |
| -------------- | ----------------------------------------------------------------------------------------------------- |
| `d_revalidate` | If lower FS has revalidate, delegate. Also check staging epoch (invalidate if commit/abort occurred). |
| `d_release`    | Free `agfs_dentry_info`.                                                                              |


---

## 7. Control Interface (ioctl on directory fd)

The control interface is exposed via ioctl on any agfs directory file
descriptor (typically `.agfs/mnt`). Directory fds on the agfs mount carry
both the standard directory operations (`iterate_shared`, `llseek`) and
the control/rule/ctl ioctl handler.

### 7.1 Ioctl Commands

```c
// Permission rule management
#define AGFS_IOC_RULE_ADD        _IOW('A', 10, struct agfs_ioc_rule)
#define AGFS_IOC_RULE_REMOVE     _IOW('A', 11, struct agfs_ioc_rule)
#define AGFS_IOC_CACHE_INVAL     _IO('A', 20)

// Control protocol (ask request / response)
#define AGFS_IOC_GET_REQUEST        _IOR('A', 30, struct agfs_ctl_request)
#define AGFS_IOC_PUT_RESPONSE       _IOW('A', 31, struct agfs_ctl_response)
```

### 7.2 Operations

| Ioctl                    | Behavior                                                                                                                                                                       |
| ------------------------ | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| `AGFS_IOC_GET_REQUEST`     | Dequeue the oldest pending permission request. Returns one `struct agfs_ctl_request` (fixed-size binary). Blocks if queue is empty (or returns `-EAGAIN` for `O_NONBLOCK`).     |
| `AGFS_IOC_PUT_RESPONSE`    | Submit a decision: one `struct agfs_ctl_response` (fixed-size binary). Wakes the sleeping thread.                                                                               |
| `AGFS_IOC_RULE_ADD`     | Add a permission rule to a dentry. Kernel resolves the path, sets `AGFS_D(dentry)->perm`, pins the dentry, and bumps `perm_gen`.                                                |
| `AGFS_IOC_RULE_REMOVE`  | Remove a rule from a dentry. Kernel sets `perm = NONE`, unpins the dentry, and bumps `perm_gen`.                                                                                |
| `AGFS_IOC_CACHE_INVAL`  | Bump `perm_gen` to invalidate all cached inode perms. Called by userspace after commit/abort.                                                                                    |

`AGFS_IOC_CACHE_INVAL` is called by userspace after commit/abort to invalidate
the kernel's dentry and inode caches so stale redirected rename dentries are
dropped and the mount reflects the new base state.

On the first `AGFS_IOC_GET_REQUEST`, a per-fd `agfs_ctl_private` is lazily
allocated to track dispatched requests. Only one daemon is allowed at a time;
a second `GET_REQUEST` from a different fd returns `EBUSY`. On fd close, any
dispatched-but-unanswered requests receive the default decision (from
`ask_default` mount option). If no daemon is connected, ask requests are
resolved immediately using `ask_default`.

---

## 8. CLI Interface

### 9.1 Commands

**Setup**

```bash
$ agfs init              # create a default agfs.toml in the current directory
```

**Full workflow** — mount, watch, exec, diff, and prompt to commit/abort in one command:

```bash
$ agfs                   # launch sh inside the sandbox
$ agfs -- make build     # run a specific command instead of sh
```

**Session management** — manual control over each step:

```bash
$ agfs mount             # create .agfs/ layout and mount the filesystem
$ agfs exec              # chroot $SHELL into .agfs/mnt (requires existing mount)
$ agfs exec -- make build
$ agfs status            # show staged changes
$ agfs diff              # git-style diff of staged vs base (rename-aware)
$ agfs commit            # apply staged changes to base
$ agfs abort             # discard staged changes
$ agfs unmount           # tear down session
```

**Permission rules and diagnostics:**

```bash
$ agfs rule add src allow-rw
$ agfs rule remove src
$ agfs watch             # handle ask requests (daemon mode)
```

### 9.2 Mount Options

Configured via `agfs.toml` or CLI flags:

| Option | Default | Description |
|---|---|---|
| `ask_timeout` | 0 (infinite) | Seconds before ask request times out |
| `ask_default` | `deny` | Fallback when no daemon is connected or on timeout |
| `noperm` | false | Disable permission gating entirely |
| `nostaging` | false | Disable staging (passthrough + gating only) |

Inside the launched shell or command, agfs `chroot`s into `.agfs/mnt` so
that the mounted view becomes `/`. The working directory remains the
caller's original CWD. For example, launching from `/home/user/project`
chroots into `.agfs/mnt` and sets the working directory to
`/home/user/project` — same absolute path, but now resolved through the
agfs mount. That is why runtime examples use absolute paths like `/src` and
`/etc` even when a rule was added as the relative path `src` from the
session root. Files under that session root are typically ruled `allow-rw`;
everything else defaults to `ask`.

---

## 9. Source File Layout

```
agfs/
├── DESIGN.md                  # This file
├── kmod/                      # Kernel module
│   ├── Makefile
│   ├── agfs.h
│   ├── super.c
│   ├── inode.c
│   ├── file.c
│   ├── dentry.c
│   ├── lookup.c
│   ├── staging.c
│   ├── perm.c
│   └── ioctl.c
└── cli/                       # Userspace CLI tool (Rust)
    ├── Cargo.toml
    └── src/
        ├── main.rs
        ├── lib.rs
        ├── config.rs          # agfs.toml management (init, rules, mount options)
        ├── mount.rs
        ├── exec.rs
        ├── unmount.rs         # `agfs unmount` — tear down session
        ├── commit.rs
        ├── abort.rs
        ├── status.rs
        ├── diff.rs
        ├── watch.rs
        └── ioctl.rs             # binary protocol structs + ioctl helpers
```

---

## 10. Lifecycle Example

```
# 1. Full interactive workflow (mount → watch + run → diff → commit/abort)
$ cd /home/user/project
$ agfs
   → creates .agfs/, mounts / → .agfs/mnt, applies rules from agfs.toml,
     starts background watch daemon for permission requests, chroots into
     .agfs/mnt, spawns $SHELL with cwd preserved as the caller's original CWD
   → on shell exit: stops watch daemon, runs `agfs diff`, prompts user to
     commit, abort, or keep staged (user runs `agfs unmount` when done)

# 1b. Or use individual commands for more control:
$ agfs mount
$ agfs watch &           # start daemon in background
$ agfs exec -- make build
$ agfs diff
$ agfs commit

# 1c. Install rules via CLI from the session root (attaches perm directly
#     to dentries)
$ agfs rule add src allow-rw
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
   → daemon: ioctl(GET_REQUEST) → agfs_ctl_request { id:1, path:"/tmp/secrets", ... }
   → daemon: decision: allow-ro
   → daemon: ioctl(PUT_RESPONSE, agfs_ctl_response { id:1, decision:ALLOW_RO })
   → kernel: wake thread, apply one-shot ALLOW_RO to this open
   → kernel: open base/tmp/secrets read-only, proceed

# 6. Agent tries to write /etc/hosts (walk up finds ALLOW_RO)
$ echo x >> /etc/hosts
   → kernel: agfs_open() → ALLOW_RO, O_WRONLY → -EACCES

# 7. Commit all staged changes to the real filesystem (userspace)
$ agfs commit
   → userspace: walk staging/, rename files to base, unlink whiteouts
   → userspace: ioctl(AGFS_IOC_CACHE_INVAL) on .agfs/mnt
   → kernel: invalidate dentry + inode caches
   → umount .agfs/mnt
```

---

## 11. Key Design Decisions

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

- On rule add/remove: `atomic_inc(&sb->perm_gen)`. All inode caches go
  stale; next `permission()` call re-resolves lazily via `d_find_alias()` +
  walk up. O(1) invalidation.
- On `AGFS_IOC_CACHE_INVAL` (after userspace commit/abort): bumps perm_gen
  and invalidates staging-related dentry caches, including pinned rename dentries.
- On `rename`: pure renames do **not** bump `perm_gen`. The inode keeps its
  `cached_perm` until some later invalidation event (rule add/remove or
  `AGFS_IOC_CACHE_INVAL`). This is intentional: rename is treated as a path
  move, not an immediate permission re-resolution point. A file moved from
  `src` under `/etc` may therefore continue to use its pre-rename effective
  permission until the next generation bump. This trades strict
  post-rename freshness for O(1) steady-state checks.

**Limitation**: directory permissions are not gated — only regular files
are checked. Directory access is controlled by standard Unix permissions on
the lower FS. This is intentional: gating directories would require
intercepting `lookup()` and `readdir()`, adding latency to every path
traversal. For agent sandboxing, controlling file-level read/write/exec is
sufficient.

---

## 12. Comparison with overlayfs

agfs borrows the whiteout convention from overlayfs but differs in
architecture and purpose.

**Staging vs live union**: overlayfs is a live union filesystem — the upper
layer *is* the persistent state. There is no commit or abort. A renamed
file is copied up to upper with `RENAME_WHITEOUT` and stays there forever.
agfs treats staging as a scratch area that is explicitly committed or
discarded. The `.renames` file tracks rename origins so commit can do
`rename(base/old, base/new)` with no data copy — something overlayfs
never needs because it never merges back to lower.

**Copy-up**: overlayfs always does a full copy-up on first write, even for
truncating writes (`echo "x" > file` copies the entire file, then
truncates). agfs detects `O_TRUNC` and creates an empty staging file
directly — zero copy for the most common agent write pattern.

**Lookup**: overlayfs does two lookups per component (upper + lower) and
merges the results. agfs does one (check staging, fall back to base).

**Permission model**: overlayfs uses standard Unix permissions only. agfs
adds the progressive gating layer (ask/allow/deny) with the ask protocol
for interactive approval.

**Portability**: overlayfs's `RENAME_WHITEOUT` requires filesystem support
(ext4, xfs). agfs uses a `.renames` sidecar file and standard `mknod()`
whiteouts, working on any lower FS.

---

## 13. Comparison with Landlock

Landlock is a Linux Security Module (LSM) for unprivileged process
sandboxing. It shares the goal of path-based access control but differs
significantly in design.

**Rule interface**: Landlock uses file descriptors to identify paths. The
userspace process opens a path with `O_PATH`, passes the fd to
`landlock_add_rule()`, and the kernel resolves it to an inode. Rules follow
the inode, not the name — immune to rename attacks. agfs uses path strings
resolved to dentries; rules are name-based and stay on the dentry.

**Rule storage**: Landlock stores rules in an rb-tree keyed by inode object
pointer, one tree per ruleset. On access, it walks up every ancestor of the
target path and does an rb-tree lookup for each — O(depth × log n). agfs
stores rules directly on dentries and caches the resolved permission on
inodes with a generation counter — O(1) in steady state.

**Overlapping rules**: Landlock is additive — rules only grant permissions.
If `/foo` has no rule and `/foo/bar` has `READ`, then `/foo/bar` is
readable but `/foo/baz` is denied. However, you **cannot** deny a child
when a parent is allowed: if `/foo` grants `READ`, then `/foo/bar` also
gets `READ` and there is no way to revoke it. agfs uses nearest-ancestor
wins: `/foo = allow-rw` + `/foo/bar = deny` works because the walk-up
finds `/foo/bar`'s rule first. Both directions (allow parent deny child,
deny parent allow child) are supported.

**Dynamic rules**: Landlock rulesets are immutable once enforced via
`landlock_restrict_self()`. You cannot add or remove rules at runtime.
agfs rules can be added, changed, or removed at any time via ioctl, with
O(1) invalidation via generation counter.

**Default policy**: Landlock is deny-by-default for "handled" access rights.
agfs is ask-by-default — unmatched paths trigger the ask protocol, which
blocks the thread until a daemon decides.

**Scope**: Landlock is per-process (attached to credentials, inherited by
children). agfs is per-mount (all processes inside the mount share the same
rules and staging area).

| Aspect | Landlock | agfs |
|---|---|---|
| Rule target | fd → inode (follows renames) | path → dentry (name-based) |
| Rule storage | rb-tree per ruleset | `perm` field on dentry |
| Access check | O(depth × log n) per ancestor | O(1) via inode cache + gen counter |
| Overlap support | Additive only (can't deny child of allowed parent) | Nearest-ancestor wins (both directions) |
| Dynamic rules | No (immutable after enforce) | Yes (add/remove/change anytime) |
| Default | Deny (handled rights) | Ask (block + prompt) |
| Scope | Per-process (cred-attached) | Per-mount |
| Staging | N/A | Full commit/abort staging layer |

# Kernel Reference

Data structures, VFS operations, control interface, and concurrency model.

## Data Structures

### Superblock Info

```c
struct agfs_sb_info {
    struct super_block     *lower_sb;
    struct path             base_path;       // always "/"
    struct path             storage_path;    // .agfs/ directory
    const struct cred      *creator_cred;    // mount-time credentials (see staging.md#credential-override-for-staging)

    // Staging
    struct path             staging_dir;       // .agfs/staging/ (flat blob store)
    struct file            *journal_file;      // .agfs/journal (opened at mount, append-only)
    struct rw_semaphore     staging_sem;       // protects staging + journal writes
    atomic64_t              next_staging_id;   // counter for staging blob IDs
    atomic64_t              snapshot_gen;      // bumped on each snapshot; triggers re-COW

    // Permission gating
    atomic64_t              perm_gen;        // bumped on rule change; invalidates inode caches
    struct list_head        pending_reqs;    // list of agfs_perm_request
    spinlock_t              pending_lock;
    wait_queue_head_t       request_waitq;   // daemon blocks here
    atomic64_t              next_req_id;
    atomic_t                has_daemon;      // 1 if a watch daemon is connected
    unsigned int            ask_timeout_s;   // seconds, 0 = infinite
    enum agfs_perm          ask_default;     // fallback on timeout
    bool                    permission;      // enable permission gating
    bool                    staging;         // enable staging area
};
```

### Inode Info

```c
struct agfs_inode_info {
    struct inode           *lower_inode;     // passthrough target

    // Permission cache (resolved at lookup, checked at permission)
    enum agfs_perm          cached_perm;
    u64                     perm_gen;        // sb->perm_gen at time of caching

    // Staging COW tracking (see staging.md#re-cow-on-first-write-after-snapshot)
    u64                     snapshot_gen;    // sbi->snapshot_gen at last COW (0 = no COW yet)

    struct inode            vfs_inode;       // embedded VFS inode
};
```

### Dentry Info

```c
struct agfs_dentry_info {
    spinlock_t              lock;
    struct path             lower_path;      // resolved lower path (staging blob or base)
    enum agfs_perm          perm;            // AGFS_PERM_NONE unless this dentry has a rule
    struct hlist_head       *ovr_buckets;    // override hash table, lazily allocated (NULL for leaf files)
};
```

Directory dentries lazily allocate a 64-bucket override hash table on
first `agfs_add_override`. Leaf file dentries keep `ovr_buckets = NULL`,
avoiding the 512-byte allocation. See [staging.md — Path Resolution](staging.md#path-resolution) for the
`agfs_override` struct.

### File Info

```c
struct agfs_file_info {
    struct file            *lower_file;     // opened lower file (base or staging)
    const struct vm_operations_struct *lower_vm_ops;
    struct agfs_ctl_private *ctl;           // non-NULL if this fd is a ctl daemon
    bool                    truncate;       // deferred O_TRUNC -> empty blob on first write
};
```

No per-fd snapshot state is needed. The COW check uses
`inode->snapshot_gen < sbi->snapshot_gen` (purely per-inode). The fsync
optimization uses `inode->snapshot_gen > 0`. The CLI enforces that
snapshots are only taken when no staging file handles are open
(see [staging.md — Re-COW](staging.md#re-cow-on-first-write-after-snapshot)),
so there are no stale cross-snapshot handles to track.

### Ctl Private (per-fd daemon state)

```c
struct agfs_ctl_private {
    struct list_head        dispatched;     // requests sent to this fd
    spinlock_t              lock;
};
```

Allocated lazily on first `AGFS_IOC_GET_REQUEST`. On fd close, any
dispatched-but-unanswered requests receive the default decision.

## Control Protocol

Fixed-size structs for the ioctl-based control interface on any AgFS directory
fd (typically `.agfs/mnt`). No parsing — just `copy_to_user()` /
`copy_from_user()`.

```c
#define AGFS_PATH_MAX 256

// kernel -> userspace: AGFS_IOC_GET_REQUEST returns one of these
struct agfs_ctl_request {
    __u64   id;
    __u32   op;                  // AGFS_OP_READ / WRITE / EXEC
    __u32   pid;
    char    comm[16];
    char    path[AGFS_PATH_MAX];
};

// userspace -> kernel: AGFS_IOC_PUT_RESPONSE accepts one of these
struct agfs_ctl_response {
    __u64   id;
    __u8    decision;            // enum agfs_perm value
    __u8    _pad[7];
};

// userspace <-> kernel: AGFS_IOC_SNAPSHOT (name in, id out)
struct agfs_ioc_snapshot {
    __u64   id;                  // out: assigned snapshot ID
    char    name[AGFS_PATH_MAX]; // in: human-readable name (NUL-terminated)
};
```

`path` is never truncated in-kernel. If the resolved mounted-view path does
not fit in `AGFS_PATH_MAX` bytes including the terminating NUL, the access
fails with `-ENAMETOOLONG` and no ask request is enqueued.

### Pending Permission Request (internal)

```c
struct agfs_perm_request {
    struct kref             ref;            // refcounted lifetime
    u64                     id;
    char                    path[AGFS_PATH_MAX];
    enum agfs_op            op;
    pid_t                   pid;
    char                    comm[TASK_COMM_LEN];

    enum agfs_perm          decision;       // set by daemon
    struct completion        done;          // thread sleeps on this
    struct list_head         list;          // sb->pending_reqs
};
```

## VFS Operations Map

### Superblock Ops

| Operation       | Behavior                                                        |
| --------------- | --------------------------------------------------------------- |
| `alloc_inode`   | Allocate `agfs_inode_info` from slab cache.                     |
| `free_inode`    | Free inode info via slab cache.                                 |
| `evict_inode`   | Clear inode, drop lower inode ref.                              |
| `statfs`        | Delegate to lower FS. Replace `f_type` with `AGFS_SUPER_MAGIC`. |
| `put_super`     | Free `agfs_sb_info`, deactivate lower super.                    |
| `show_options`  | Print mount options.                                            |

### Inode Ops (Directory)

| Operation    | Perm check                                                   | Staging layer                                                                     | Passthrough                               |
| ------------ | ------------------------------------------------------------ | --------------------------------------------------------------------------------- | ----------------------------------------- |
| `lookup`     | --                                                           | Check override table first (deleted -> ENOENT, staging_id -> blob, base_path -> redirect); fall back to base. | `lookup_one_len()` on base dir. |
| `create`     | -- (dir perm via lower FS)                                   | Allocate staging blob, add override + journal append.                           | `vfs_create()` on staging blob. |
| `mkdir`      | -- (dir perm via lower FS)                                   | Allocate staging dir, add override + journal append.                            | --                               |
| `unlink`     | -- (dir perm via lower FS)                                   | Add DELETED override, journal append.                                           | --                                         |
| `rmdir`      | -- (dir perm via lower FS)                                   | Add DELETED override, journal append.                                           | --                                         |
| `rename`     | -- (dir perm via lower FS)                                   | See [Rename Handling](staging.md#rename-handling).                               | --                                         |
| `symlink`    | -- (dir perm via lower FS)                                   | Allocate staging blob (symlink), add override + journal append.                 | `vfs_symlink()`.                          |
| `permission` | **Gating for regular files (O(1) cached); delegate to lower FS for dirs.** | --                                                                                 | `inode_permission()` on lower inode.      |
| `setattr`    | Gated (regular files only).                                  | Copy base->staging first, then setattr on staging.                                 | `notify_change()` on lower.               |
| `getattr`    | Gated (regular files only).                                  | Stat from resolved path (staging or base).                                        | `vfs_getattr()` on lower.                 |

### File Ops

| Operation    | Behavior                                                                                                                                                                                          |
| ------------ | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `open`       | Perm gating (via dentry). If writable and staging file exists: open staging file. If writable and no staging file: open base read-only (COW on first write). If read-only: open resolved lower file. |
| `read_iter`  | Swap `kiocb->ki_filp` to lower file, call `lower->read_iter()`.                                                                                                                                   |
| `write_iter` | If `inode->snapshot_gen < sbi->snapshot_gen`: unified COW/re-COW via `agfs_do_cow` (source = dentry's `lower_path`). Then delegate to `lower->write_iter()`. |
| `mmap`       | If `inode->snapshot_gen < sbi->snapshot_gen` and mapping is writable+shared: trigger COW/re-COW. Then delegate to lower file. |
| `fsync`      | If `inode->snapshot_gen > 0`: return 0 (staging files are ephemeral). Otherwise delegate to lower.                                                            |
| `release`    | `fput()` lower file. Free `agfs_file_info`.                                                                                                                                                       |
| `llseek`     | Delegate to lower.                                                                                                                                                                                |

### Dentry Ops

| Operation      | Behavior                                                                                              |
| -------------- | ----------------------------------------------------------------------------------------------------- |
| `d_revalidate` | If lower FS has revalidate, delegate. Also check staging epoch (invalidate if commit/abort occurred). |
| `d_release`    | Free `agfs_dentry_info`.                                                                              |

## Control Interface (ioctl)

The control interface is exposed via ioctl on any AgFS directory file
descriptor (typically `.agfs/mnt`). Directory fds on the AgFS mount carry
both the standard directory operations (`iterate_shared`, `llseek`) and
the control/rule/ctl ioctl handler.

### Ioctl Commands

```c
// Permission rule management
#define AGFS_IOC_RULE_ADD        _IOW('A', 10, struct agfs_ioc_rule)
#define AGFS_IOC_RULE_REMOVE     _IOW('A', 11, struct agfs_ioc_rule)
#define AGFS_IOC_CACHE_INVAL     _IO('A', 20)

// Control protocol (ask request / response)
#define AGFS_IOC_GET_REQUEST        _IOR('A', 30, struct agfs_ctl_request)
#define AGFS_IOC_PUT_RESPONSE       _IOW('A', 31, struct agfs_ctl_response)

// Snapshot
#define AGFS_IOC_SNAPSHOT           _IOWR('A', 40, struct agfs_ioc_snapshot)
```

### Ioctl Behavior

| Ioctl                    | Behavior                                                                                                                                                                       |
| ------------------------ | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| `AGFS_IOC_GET_REQUEST`     | Dequeue the oldest pending permission request. Returns one `struct agfs_ctl_request` (fixed-size binary). Blocks if queue is empty (or returns `-EAGAIN` for `O_NONBLOCK`).     |
| `AGFS_IOC_PUT_RESPONSE`    | Submit a decision: one `struct agfs_ctl_response` (fixed-size binary). Wakes the sleeping thread.                                                                               |
| `AGFS_IOC_RULE_ADD`     | Add a permission rule to a dentry. Kernel resolves the path, sets `AGFS_D(dentry)->perm`, pins the dentry, and bumps `perm_gen`.                                                |
| `AGFS_IOC_RULE_REMOVE`  | Remove a rule from a dentry. Kernel sets `perm = NONE`, unpins the dentry, and bumps `perm_gen`.                                                                                |
| `AGFS_IOC_CACHE_INVAL`  | Bump `perm_gen`, shrink dentry/inode caches, and reopen the journal file. Called by userspace after commit/abort.                                                           |
| `AGFS_IOC_SNAPSHOT`     | Bump `snapshot_gen`, append `S` record to journal, return snapshot ID. Triggers lazy re-COW on next write to any staged file (see [staging.md — Snapshots](staging.md#snapshot-mechanism)). |

`AGFS_IOC_CACHE_INVAL` is called by userspace after commit/abort. It:
1. Bumps `perm_gen` to invalidate all cached inode permissions.
2. Calls `shrink_dcache_sb()` to drop stale dentry caches so the mount
   reflects the new base state.
3. Closes and reopens the journal file (the CLI deletes it on
   commit/abort, so the old fd is stale).

On the first `AGFS_IOC_GET_REQUEST`, a per-fd `agfs_ctl_private` is lazily
allocated to track dispatched requests. Only one daemon is allowed at a time;
a second `GET_REQUEST` from a different fd returns `EBUSY`. On fd close, any
dispatched-but-unanswered requests receive the default decision (from
`ask_default` mount option). If no daemon is connected, ask requests are
resolved immediately using `ask_default`.

## Concurrency

| Lock | Protects | Type |
|---|---|---|
| `sb->staging_sem` | Publishing staging mutations atomically (override + journal + dentry swap + `inode->snapshot_gen`) | `rw_semaphore` (write for rename/COW/truncate-open). Create/mkdir/symlink/unlink/rmdir are serialized by VFS `inode_lock(dir)` and do not need `staging_sem`. |
| `sb->pending_lock` | Pending request queue | `spinlock` |
| `dentry_info->lock` | Per-directory override table + cached lower path | `spinlock` |

**Lock ordering**: `staging_sem` -> `pending_lock` -> `dentry_info->lock`

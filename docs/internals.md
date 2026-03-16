# Kernel Reference

VFS operations, control interface, and concurrency model.

All struct definitions live in [`kmod/agfs.h`](../kmod/agfs.h).

## Design Notes

Directory inodes lazily allocate a 64-bucket dirent hash table on
first `agfs_add_dirent`. Non-directory inodes keep `de_buckets = NULL`,
avoiding the 512-byte allocation. See [staging.md — Path Resolution](staging.md#path-resolution) for the
`agfs_dirent` struct.

No per-fd snapshot state is needed. The COW check uses
`dirent.snapshot_gen < sbi->snapshot_gen` (purely per-dirent). The fsync
optimization uses `dirent.ino > 0`. The CLI enforces that
snapshots are only taken when no staging file handles are open
(see [staging.md — Re-COW](staging.md#re-cow-on-first-open-for-write-after-snapshot)),
so there are no stale cross-snapshot handles to track.

## Control Protocol

The ioctl-based control interface uses fixed-size binary structs (see
`agfs_ctl_request`, `agfs_ctl_response`, and `agfs_ioc_snapshot` in
[`agfs.h`](../kmod/agfs.h)) — no parsing, just `copy_to_user()` /
`copy_from_user()`.

`path` is never truncated in-kernel. If the resolved mounted-view path does
not fit in `AGFS_PATH_MAX` bytes including the terminating NUL, the access
fails with `-ENAMETOOLONG` and no ask request is enqueued.

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
| `lookup`     | --                                                           | Check dirent table first (deleted -> ENOENT, ino -> staged inode, base_path -> redirect); fall back to base. | `lookup_one_len()` on base dir. |
| `create`     | -- (dir perm via lower FS)                                   | Allocate inode, add dirent + journal append.                                  | `vfs_create()` on inode store.  |
| `mkdir`      | -- (dir perm via lower FS)                                   | Allocate directory inode, add dirent + journal append.                        | --                               |
| `unlink`     | -- (dir perm via lower FS)                                   | Add DELETED dirent, journal append.                                           | --                                         |
| `rmdir`      | -- (dir perm via lower FS)                                   | Add DELETED dirent, journal append.                                           | --                                         |
| `rename`     | -- (dir perm via lower FS)                                   | See [Rename Handling](staging.md#rename-handling).                               | --                                         |
| `symlink`    | -- (dir perm via lower FS)                                   | Allocate inode (symlink), add dirent + journal append.                        | `vfs_symlink()`.                          |
| `permission` | **Gating for regular files (O(1) cached); delegate to lower FS for dirs.** | --                                                                                 | `inode_permission()` on lower inode.      |
| `setattr`    | Gated (regular files only).                                  | Setattr on resolved lower file (staged inode or base). No COW triggered.           | `notify_change()` on lower.               |
| `getattr`    | Gated (regular files only).                                  | Stat from resolved path (staged inode or base).                                   | `vfs_getattr()` on lower.                 |

### File Ops

| Operation    | Behavior                                                                                                                                                                                          |
| ------------ | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `open`       | Perm gating (via dentry). If writable and inode is current: open staged inode. If writable and needs COW: perform COW at open time. If read-only: open resolved lower file. |
| `read_iter`  | Swap `kiocb->ki_filp` to lower file, call `lower->read_iter()`.                                                                                                                                   |
| `write_iter` | Pure pass-through — COW already resolved at open time. Delegate to `lower->write_iter()`. |
| `mmap`       | Pure pass-through — COW already resolved at open time. Delegate to lower file. |
| `fsync`      | If dirent has `ino > 0`: return 0 (staged inodes are ephemeral). Otherwise delegate to lower.                                                            |
| `release`    | Decrement `staging_fd_count` if write-mode. `fput()` lower file. Free `agfs_file_info`.                                                                                                           |
| `llseek`     | Delegate to lower.                                                                                                                                                                                |

### Dentry Ops

| Operation      | Behavior                                                                                              |
| -------------- | ----------------------------------------------------------------------------------------------------- |
| `d_revalidate` | If lower FS has revalidate, delegate. Also check staging epoch (invalidate if commit/abort occurred). |
| `d_release`    | Free `agfs_dentry_info`.                                                                              |

## Control Interface (ioctl)

The control interface is exposed via ioctl on any AgFS directory file
descriptor (typically `.agfs/mnt`). Ioctl command macros are defined in
[`agfs.h`](../kmod/agfs.h).

### Ioctl Behavior

| Ioctl                    | Behavior                                                                                                                                                                       |
| ------------------------ | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| `AGFS_IOC_GET_REQUEST`     | Dequeue the oldest pending permission request. Returns one `struct agfs_ctl_request` (fixed-size binary). Blocks if queue is empty (or returns `-EAGAIN` for `O_NONBLOCK`).     |
| `AGFS_IOC_PUT_RESPONSE`    | Submit a decision: one `struct agfs_ctl_response` (fixed-size binary). Wakes the sleeping thread.                                                                               |
| `AGFS_IOC_RULE_ADD`     | Add a permission rule to a dentry. Kernel resolves the path, sets `AGFS_D(dentry)->perm`, pins the dentry, and bumps `perm_gen`.                                                |
| `AGFS_IOC_RULE_REMOVE`  | Remove a rule from a dentry. Kernel sets `perm = NONE`, unpins the dentry, and bumps `perm_gen`.                                                                                |
| `AGFS_IOC_CACHE_INVAL`  | Bump `perm_gen`, release pinned directory inodes, shrink dentry/inode caches, and reopen the journal file. Called by userspace after commit/abort.                                                           |
| `AGFS_IOC_SNAPSHOT`     | Bump `snapshot_gen`, append `S` record to journal, return snapshot ID. Rejects with `-EBUSY` if staging fds are open. Triggers re-COW on next open-for-write to any staged file (see [staging.md — Snapshots](staging.md#snapshot-mechanism)). |

`AGFS_IOC_CACHE_INVAL` is called by userspace after commit/abort. It:
1. Bumps `perm_gen` to invalidate all cached inode permissions.
2. Walks `pinned_dirs`, frees dirent tables, and calls `iput()` on each
   pinned directory inode.
3. Calls `shrink_dcache_sb()` to drop stale dentry caches so the mount
   reflects the new base state.
4. Closes and reopens the journal file (the CLI deletes it on
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
| `sb->staging_sem` | Publishing staging mutations atomically (dirent + journal + dentry swap + `dirent.snapshot_gen`) | `rw_semaphore` (write for COW/snapshot). Create/mkdir/symlink/unlink/rmdir/rename are serialized by VFS `inode_lock(dir)` and do not need `staging_sem`. |
| `sb->pending_lock` | Pending request queue | `spinlock` |
| `inode->i_rwsem` (VFS) | Per-directory dirent table | `rw_semaphore` (held by VFS for lookup/readdir/mutations) |
| `dentry_info->lock` | Cached lower path | `spinlock` |

**Lock ordering**: `staging_sem` -> `inode->i_rwsem` -> `pending_lock` -> `dentry_info->lock`

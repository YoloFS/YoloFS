# Kernel Reference

VFS operations, control interface, and concurrency model.

All struct definitions live in [`kmod/agfs.h`](../kmod/agfs.h).

## Design Notes

Directory inodes lazily allocate a 64-bucket dirent hash table on
first `agfs_add_dirent`. Non-directory inodes keep `de_buckets = NULL`,
avoiding the 512-byte allocation. See [staging.md — Path Resolution](staging.md#path-resolution) for the
`agfs_dirent` struct.

No per-fd checkpoint state is needed. The COW check uses
`dirent.checkpoint_gen < sbi->checkpoint_gen` (purely per-dirent). The fsync
optimization uses `agfs_ino_is_staged(de->ino)`. The CLI enforces that
checkpoints are only taken when no staging file handles are open
(see [staging.md — Re-COW](staging.md#re-cow-on-first-open-for-write-after-checkpoint)),
so there are no stale cross-checkpoint handles to track.

## Control Protocol

The ioctl-based control interface uses small fixed-size binary headers
with pointer+length fields for variable-length paths (see struct
definitions in [`agfs.h`](../kmod/agfs.h)). Path data is transferred
via secondary `copy_from_user()` / `copy_to_user()` calls, keeping the
ioctl structs compact regardless of path length. Paths are limited to
`AGFS_PATH_MAX` (256) bytes including the terminating NUL.

`path` is never truncated in-kernel. If the resolved mounted-view path does
not fit in `AGFS_PATH_MAX` (256) bytes including the terminating NUL, the
access fails with `-ENAMETOOLONG` and no ask request is enqueued. Note that
the internal kernel path buffer is smaller than the `PATH_MAX` limit used by
the ioctl wire format.

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
| `create`     | -- (dir perm via lower FS)                                   | Allocate inode, add dirent + journal E record.                                  | `vfs_create()` on inode store.  |
| `mkdir`      | -- (dir perm via lower FS)                                   | Allocate directory inode, add dirent + journal E record.                        | --                               |
| `unlink`     | -- (dir perm via lower FS)                                   | Add DELETED dirent, journal E record.                                           | --                                         |
| `rmdir`      | -- (dir perm via lower FS)                                   | Add DELETED dirent, journal E record.                                           | --                                         |
| `rename`     | -- (dir perm via lower FS)                                   | See [Rename Handling](staging.md#rename-handling). Emits 2 E records.             | --                                         |
| `symlink`    | -- (dir perm via lower FS)                                   | Allocate inode (symlink), add dirent + journal E record.                        | `vfs_symlink()`.                          |
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
| `fsync`      | If `agfs_ino_is_staged(de->ino)`: return 0 (staged inodes are ephemeral). Otherwise delegate to lower.                                                            |
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
| `AGFS_IOC_GET_REQUEST`     | Dequeue the oldest pending permission request. Userspace passes a `struct agfs_ctl_request` with a path buffer pointer and capacity; kernel fills in the request fields and writes the path into the buffer. Blocks if queue is empty (or returns `-EAGAIN` for `O_NONBLOCK`). |
| `AGFS_IOC_PUT_RESPONSE`    | Submit a decision: one `struct agfs_ctl_response`. Wakes the sleeping thread.                                                                               |
| `AGFS_IOC_RULE_ADD`     | Add a permission rule to a dentry. Kernel resolves the path, sets `AGFS_D(dentry)->perm`, pins the dentry, and bumps `perm_gen`.                                                |
| `AGFS_IOC_RULE_REMOVE`  | Remove a rule from a dentry. Kernel sets `perm = NONE`, unpins the dentry, and bumps `perm_gen`.                                                                                |
| `AGFS_IOC_RESTORE`    | Atomically reset staging state and optionally inject dirent entries. Called by userspace after commit/abort (with `entry_count=0`) and for restore (with entries). Rejects with `-EBUSY` if staging fds are open. See detailed steps below and [staging.md — Restore](staging.md#checkpoint-aware-cli-operations). |
| `AGFS_IOC_CHECKPOINT`     | Bump `checkpoint_gen`, append `K` record to journal, return checkpoint ID. Rejects with `-EBUSY` if staging fds are open. Triggers re-COW on next open-for-write to any staged file (see [staging.md — Checkpoints](staging.md#checkpoint-mechanism)). |

`AGFS_IOC_RESTORE` is called by userspace after commit/abort (with
`entry_count=0`, `checkpoint_gen=1` to reset to initial state) and for
restore (with entries to rebuild the staging view at a given checkpoint).
It:
1. Takes `staging_sem` write lock; rejects with `-EBUSY` if
   `staging_fd_count > 0`.
2. Bumps `perm_gen` to invalidate all cached inode permissions.
3. Walks `pinned_dirs`, frees dirent tables, and calls `iput()` on each
   pinned directory inode.
4. Calls `shrink_dcache_sb()` to drop stale dentry caches so the mount
   reflects the new base state.
5. For each entry in the userspace array: resolves the parent path
   via `vfs_path_lookup` on the mount root, takes `inode_lock(parent)`,
   calls `agfs_add_dirent()` to install the dirent, then releases.
6. Sets `checkpoint_gen` to the requested value (only on success).

On the first `AGFS_IOC_GET_REQUEST`, a per-fd `agfs_ctl_private` is lazily
allocated to track dispatched requests. Only one daemon is allowed at a time;
a second `GET_REQUEST` from a different fd returns `EBUSY`. On fd close, any
dispatched-but-unanswered requests receive the default decision (from
`ask_default` mount option). If no daemon is connected, ask requests are
resolved immediately using `ask_default`.

## Concurrency

| Lock | Protects | Type |
|---|---|---|
| `sb->staging_sem` | Publishing staging mutations atomically (dirent + journal + dentry swap + `dirent.checkpoint_gen`). Also serializes restore. | `rw_semaphore` (write for COW/checkpoint/restore). Create/mkdir/symlink/unlink/rmdir/rename are serialized by VFS `inode_lock(dir)` and do not need `staging_sem`. |
| `sb->pending_lock` | Pending request queue | `spinlock` |
| `inode->i_rwsem` (VFS) | Per-directory dirent table | `rw_semaphore` (held by VFS for lookup/readdir/mutations) |
| `dentry_info->lock` | Cached lower path | `spinlock` |

**Lock ordering**: `staging_sem` -> `inode->i_rwsem` -> `pending_lock` -> `dentry_info->lock`

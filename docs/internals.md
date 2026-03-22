# Kernel Reference

VFS operations, control interface, and concurrency model.

All struct definitions live in [`kmod/agfs.h`](../kmod/agfs.h).

## Design Notes

Directory inodes maintain a linked list of pinned staged child dentries
(`de_list`). Non-directory inodes keep `de_list` empty
(`INIT_LIST_HEAD(&de_list)`). The packed overlay state (`agfs_pde_t`) is
stored directly on each VFS dentry via `d_fsdata` (in `agfs_dentry_info`),
and staged dentries are pinned with `dget()` to guarantee lifetime.
See [staging.md — Path Resolution](staging.md#path-resolution) for
the packed encoding.

No per-fd checkpoint state is needed. The COW check uses
`agfs_pde_gen(packed) < sbi->gen` (purely per-dentry). The fsync
optimization checks `sbi->staging && S_ISREG && FMODE_WRITE` — staged
writable files are ephemeral and skip fsync entirely. The CLI enforces that
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
| `lookup`     | --                                                           | Staged entries are pinned in dcache (`lookup_fast` finds them); `->lookup` only handles unstaged names — base-only lookup. | `lookup_one_len()` on base dir. |
| `create`     | -- (dir perm via lower FS)                                   | Allocate inode, set packed on dentry, pin with `dget()`, add to `de_list` + journal A record. | `vfs_create()` on inode store.  |
| `mkdir`      | -- (dir perm via lower FS)                                   | Allocate directory inode, set packed on dentry, pin with `dget()`, add to `de_list` + journal A record. | --                               |
| `unlink`     | -- (dir perm via lower FS)                                   | Create tombstone dentry (`d_alloc` + `d_add(NULL)`), add to `de_list`, journal D record. `d_alloc` ref is the pin. | --                                         |
| `rmdir`      | -- (dir perm via lower FS)                                   | Create tombstone dentry (`d_alloc` + `d_add(NULL)`), add to `de_list`, journal D record. `d_alloc` ref is the pin. | --                                         |
| `rename`     | -- (dir perm via lower FS)                                   | See [Rename Handling](staging.md#rename-handling). Sets packed on dentries, pins with `dget()`. Emits single R or P record. | --                                         |
| `symlink`    | -- (dir perm via lower FS)                                   | Allocate inode (symlink), set packed on dentry, pin with `dget()`, add to `de_list` + journal A record. | `vfs_symlink()`.                          |
| `permission` | **Gating for regular files (O(1) cached); delegate to lower FS for dirs.** | --                                                                                 | `inode_permission()` on lower inode.      |
| `setattr`    | Gated (regular files only).                                  | Setattr on resolved lower file (staged inode or base). No COW triggered.           | `notify_change()` on lower.               |
| `getattr`    | Gated (regular files only).                                  | Stat from resolved path (staged inode or base).                                   | `vfs_getattr()` on lower.                 |

### File Ops

| Operation    | Behavior                                                                                                                                                                                          |
| ------------ | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `open`       | Perm gating (via dentry). If writable and inode is current: open staged inode. If writable and needs COW: perform COW at open time. If read-only: open resolved lower file. |
| `read_iter`  | Swap `kiocb->ki_filp` to lower file, call `lower->read_iter()`.                                                                                                                                   |
| `write_iter` | Pure pass-through — COW already resolved at open time. Delegate to `lower->write_iter()`. |
| `fallocate`  | Pure pass-through to `lower->f_op->fallocate()` when supported by the lower fs. |
| `mmap`       | Pure pass-through — COW already resolved at open time. Delegate to lower file. |
| `fsync`      | If staging enabled, regular file, and opened for write: return 0 (staged writable files are ephemeral). Otherwise delegate to lower.                                                            |
| `release`    | Decrement `staging_fd_count` if write-mode. `fput()` lower file. Free `agfs_file_info`.                                                                                                           |
| `llseek`     | Delegate to lower.                                                                                                                                                                                |

### Dentry Ops

| Operation      | Behavior                                                                                              |
| -------------- | ----------------------------------------------------------------------------------------------------- |
| `d_init`       | Auto-initialize `agfs_dentry_info` on every dentry at allocation time. Sets up `d_fsdata`, `INIT_LIST_HEAD(&de_node)`, zeroes `packed`. Replaces manual `agfs_new_dentry_private_data()`. |
| `d_revalidate` | If lower FS has revalidate, delegate. Also check staging epoch (invalidate if commit/abort occurred). |
| `d_release`    | Warn if dentry is still on a `de_list` (`!list_empty(&de_node)`). Free link pointer if packed is a link. Free `agfs_dentry_info`. |

## Control Interface (ioctl)

The control interface is exposed via ioctl on any AgFS directory file
descriptor (typically `.agfs/mnt`). Ioctl command macros are defined in
[`agfs.h`](../kmod/agfs.h).

### Ioctl Behavior

| Ioctl                    | Behavior                                                                                                                                                                       |
| ------------------------ | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| `AGFS_IOC_GET_REQUEST`     | Dequeue the oldest pending permission request. Userspace passes a `struct agfs_ctl_request` with a path buffer pointer and capacity; kernel fills in the request fields and writes the path into the buffer. Blocks if queue is empty (or returns `-EAGAIN` for `O_NONBLOCK`). Returns `-EBUSY` if another daemon is already connected. Returns `-EOVERFLOW` if the path exceeds the supplied buffer capacity. |
| `AGFS_IOC_PUT_RESPONSE`    | Submit a decision: one `struct agfs_ctl_response`. Wakes the sleeping thread. Returns `-EINVAL` if the caller is not the connected daemon or if `decision > AGFS_PERM_DENY`. Returns `-ENOENT` if the request ID is not found in the dispatched list. |
| `AGFS_IOC_RULE_ADD`     | Add a permission rule to a dentry. Kernel resolves the path, sets `AGFS_D(dentry)->perm`, pins the dentry, and bumps `perm_gen`. Returns `-EINVAL` if `perm > AGFS_PERM_DENY`. Returns `-EXDEV` if the path is on a different superblock. |
| `AGFS_IOC_RULE_REMOVE`  | Remove a rule from a dentry. Kernel sets `perm = NONE`, unpins the dentry, and bumps `perm_gen`. Returns `-EXDEV` if the path is on a different superblock. |
| `AGFS_IOC_RESTORE`    | Atomically reset staging state and optionally inject staged VFS dentries. Two modes: reset (`target_gen=0`) for commit/abort, restore (`target_gen>0`) for restore. Restore mode: increments `gen`, injects dentries with new gen, appends T record to journal, returns `new_gen`. Reset mode: releases staged dentries, sets `gen=0`, no journal write. Ioctl is `_IOWR` to return `new_gen`. Returns `-EBUSY` if staging fds are open. Returns `-EOPNOTSUPP` if staging is not enabled. Returns `-EOVERFLOW` if `gen` would exceed `U16_MAX`. Returns `-EINVAL` for malformed tree data. See detailed steps below and [staging.md — Restore](staging.md#checkpoint-aware-cli-operations). |
| `AGFS_IOC_CHECKPOINT`     | Bump `gen`, append K record to journal, return checkpoint ID. Returns `-EBUSY` if staging fds are open. Returns `-EOPNOTSUPP` if staging is not enabled. Returns `-EOVERFLOW` if `gen` would exceed `U16_MAX`. Triggers re-COW on next open-for-write to any staged file (see [staging.md — Checkpoints](staging.md#checkpoint-mechanism)). If `AGFS_CHK_IF_CHANGED` flag is set and no data has changed since the last checkpoint/restore, returns success with `gen=0` (checkpoint skipped). |

`AGFS_IOC_RESTORE` is called by userspace after commit/abort (with
`tree_len=0`, `tree_ptr=0`, `target_gen=0` to reset) and for restore (with a
serialized DirTree buffer to rebuild the staging view at a given checkpoint,
`target_gen>0`).
The ioctl struct has fields `target_gen`, `new_gen`, `tree_len`, and
`tree_ptr`.
It:
1. Takes `staging_sem` write lock; rejects with `-EBUSY` if
   `staging_fd_count > 0`.
2. Bumps `perm_gen` to invalidate all cached inode permissions.
3. Walks `pinned_dirs`, iterates each directory's `de_list`, removes each
   child dentry from the list and calls `dput()` to release the pin. No
   `iput()` needed — child `dput()` cascades to the parent via VFS
   `d_parent` refcounting.
4. Calls `shrink_dcache_sb()` to drop stale dentry caches so the mount
   reflects the new base state.
5. **Reset mode** (`target_gen=0`): Sets `sbi->gen` to 0 and returns.
   Steps 6–9 are skipped.
6. **Restore mode** (`target_gen>0`): Increments `sbi->gen` to allocate
   `new_gen`.
7. **Restore mode**: `vmalloc`s + `copy_from_user`s the serialized tree
   buffer, then walks it iteratively with an explicit directory stack.
   For each node: reads the name, optional packed value, and child
   count. If a packed value is present, creates a VFS dentry via `d_alloc()`
   (`d_init` sets up `d_fsdata`), resolves the staged inode, sets
   `packed` with `gen = new_gen`, calls `d_add(dentry, inode)` (or
   `d_add(dentry, NULL)` for tombstones), uses the `d_alloc()` reference
   as pin (no extra `dget()`), and adds to parent's `de_list`. If children are present, `lookup_one_len`
   finds the child directory dentry and pushes onto the stack.
8. **Restore mode**: Appends T record (`T\0<new_gen>\0<target_gen>\n`) to journal.
9. **Restore mode**: Writes back `new_gen` to userspace struct.

On the first `AGFS_IOC_GET_REQUEST`, a per-fd `agfs_ctl_private` is lazily
allocated to track dispatched requests. Only one daemon is allowed at a time;
a second `GET_REQUEST` from a different fd returns `EBUSY`. On fd close, any
dispatched-but-unanswered requests receive the default decision (from
`ask_default` mount option). If no daemon is connected, ask requests are
resolved immediately using `ask_default`.

## Concurrency

| Lock | Protects | Type |
|---|---|---|
| `sb->staging_sem` | Publishing staging mutations atomically (packed + journal + dentry swap + packed gen). Also serializes restore (T record write). | `rw_semaphore` (write for COW/checkpoint/restore). Create/mkdir/symlink/unlink/rmdir/rename are serialized by VFS `inode_lock(dir)` and do not need `staging_sem`. |
| `sb->pending_lock` | Pending request queue | `spinlock` |
| `inode->i_rwsem` (VFS) | Per-directory `de_list` (pinned staged child dentries) | `rw_semaphore` (held by VFS for lookup/readdir/mutations) |
| `dentry_info->lock` | Cached lower path | `spinlock` |

**Lock ordering**: `staging_sem` -> `inode->i_rwsem` -> `pending_lock` -> `dentry_info->lock`

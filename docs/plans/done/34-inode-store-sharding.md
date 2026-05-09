# 34 — Inode store sharding

## Problem

The inode store (`.yolofs/inodes/`) is a single flat directory. Every
`yolo_inode_alloc` call does `lookup_one_len` + `vfs_create` in this
directory. Both operations search the ext4 htree, which grows with the
number of staged files.

Profiling linux-untar (~80K files) shows `yolo_inode_alloc` at 88% of
`yolo_create` inclusive time. The `lookup_one_len` on line 55 of
`staging.c` is the dominant cost — it does a full negative lookup in the
flat directory (the inode name is always fresh, so the lookup always
fails before the create redoes the search).

At 80K entries the ext4 htree is ~3 levels deep. The cost is O(log N)
per create but with high constant factors (buffer_head lookups, journal
handles, hash computation). This makes yolofs create ~9µs/file slower than
overlayfs, which creates files in the natural directory tree.

## Approach

Shard the inode store into subdirectories based on inode number:

```
.yolofs/inodes/<shard>/<ino>
```

where `<shard> = ino / SHARD_SIZE` (e.g., SHARD_SIZE = 1000).

Each shard directory holds at most SHARD_SIZE entries, keeping ext4
htree at 1 level. Shard directories are created on demand.

## Changes

### kmod/staging.c — `yolo_inode_alloc`

- Compute `shard = ino / SHARD_SIZE`.
- Look up or create the shard directory under `sbi->inodes_dir`.
- `lookup_one_len` + `vfs_create` in the shard directory instead of
  the root inodes directory.
- Cache the current shard dentry in `sbi` to avoid repeated lookups
  (sequential inos hit the same shard ~1000 times in a row).

### cli/ — inode path resolution

- `utils::inode_path()` must produce `inodes/<shard>/<ino>` instead
  of `inodes/<ino>`.
- Commit, abort, restore, diff, and status all use `inode_path()` so
  they pick up the change automatically.

### cli/ — abort cleanup

- `abort::reset_staging()` removes all shard directories under
  `inodes/`, not just flat files.

### Migration

- On mount, check whether `inodes/` contains flat files (old layout)
  or shard directories (new layout). Support both during a transition
  period, or require a commit+remount to migrate.

## Expected impact

At 80K files, each shard has ≤1000 entries (1-level htree). The
`lookup_one_len` cost drops from ~3-level htree search to ~1-level.
This should close most of the 9µs/file gap vs overlayfs on
create-heavy workloads.

# 25 — Remove `pinned_dirs` and `de_list`

## Problem

The kmod maintains two custom linked lists for dentry staging that can be
replaced by VFS-native structures:

1. **`de_list` / `de_node`** — per-directory list of staged children.  Used
   for readdir merge (emit staged entries, deduplicate base entries) and bulk
   cleanup.  Since staged dentries are `dget()`-pinned, they already persist
   in the VFS `d_children` list.  We can walk `d_children` and filter by
   `dstate.val != 0` (i.e., not passthrough) instead.

2. **`pinned_dirs` / `de_pin` / `pinned_dirs_lock`** — global list of
   directories with staged children.  Only used for cleanup during restore
   and unmount (`yolo_release_pinned_dirs()`).  Can be replaced by a
   recursive dentry tree walk from `sb->s_root`, checking `d_children`
   for staged entries.

This removes five struct fields, one spinlock, one function
(`yolo_pin_dir_if_first()`), and simplifies six call-sites.

## Approach

- Replace `!list_empty(&di->de_node)` "is staged" test with
  `!yolo_dstate_is_passthrough(di->dstate)` (equivalently `di->dstate.val != 0`).
- Readdir emit (phase 1): walk `d_children` + `dstate.val != 0` filter.
- Readdir dedup (phase 2): use `d_lookup()` per base entry — O(1) dcache
  hash lookup, strictly better than the current O(staged) `de_list` scan.
- Bulk cleanup: replace `pinned_dirs` iteration with a recursive dentry
  tree walk from `sb->s_root`.
- Drop unused `dir` parameter from `yolo_stage_dentry()` and
  `yolo_remove_tombstone()`.
- Drop the readdir fast-path `de_list` emptiness check — the remaining
  `!sbi->staging || !sbi->inodes_dir.dentry` check handles the common
  case.  The slow path is already efficient for dirs with no staged entries.
- Locking: `d_children` is an `hlist_head` protected by `dentry->d_lock`.
  `libfs.c:dcache_readdir()` and `afs/dynroot.c` walk it under `i_rwsem`.
  YoloFS readdir already holds `i_rwsem` (shared) via VFS `iterate_shared`,
  so the pattern is compatible.  To be safe, acquire `d_lock` around
  `d_children` traversals.

## Encoding

`dstate.val == 0` means passthrough (default).  All three staged states have
`dstate.val != 0`:

- Tombstone: `d_type << 60 | 1 << 59` (ino=0, in_base=1)
- Staged inode: `d_type << 60 | in_base << 59 | ino << 16 | gen`
- Base path: `1 << 63 | d_type << 60 | in_base << 59 | ptr`

Filtering `d_children` by `dstate.val != 0` cleanly identifies staged
entries with no ambiguity.

## Fields removed

| Struct | Field | Purpose |
|---|---|---|
| `yolo_sb_info` | `pinned_dirs` | Global list of dirs with staged children |
| `yolo_sb_info` | `pinned_dirs_lock` | Spinlock protecting above |
| `yolo_inode_info` | `de_list` | Per-dir list of staged children |
| `yolo_inode_info` | `de_pin` | Node in `sbi->pinned_dirs` |
| `yolo_dentry_info` | `de_node` | Node in parent's `de_list` |

## Functions removed

- `yolo_pin_dir_if_first()` (dentry.c) — no longer needed; nothing to pin to.

## Signature changes

- `yolo_stage_dentry()`: drop `dir` parameter (was only used for
  `list_add` and `yolo_pin_dir_if_first`).  Becomes a 2-liner: set
  dstate + `dget()`.
- `yolo_add_tombstone()`: drop `dir` parameter (was only used for
  `list_add` to `de_list` and `yolo_pin_dir_if_first`).
- `yolo_remove_tombstone()`: drop `dir` parameter (was only used for
  `list_del_init`).
- `yolo_emit_dirents()`: change from `struct inode *dir` to
  `struct dentry *parent` (needs dentry for `d_children` walk; caller
  passes `file_dentry(file)`).
- `yolo_release_pinned_dirs()`: change from `(struct yolo_sb_info *sbi)`
  to `(struct super_block *sb)`.

## Changes

### 1. yolofs.h — struct changes and declarations

- Remove `de_list` from `yolo_inode_info` (line 356).
- Remove `de_pin` from `yolo_inode_info` (line 357).
- Remove `de_node` from `yolo_dentry_info` (line 368).
- Remove `pinned_dirs` from `yolo_sb_info` (line 335).
- Remove `pinned_dirs_lock` from `yolo_sb_info` (line 336).
- Remove `yolo_pin_dir_if_first()` declaration (lines 513–514).

### 2. super.c — init and eviction

- `yolo_alloc_inode()`: remove `INIT_LIST_HEAD(&i->de_list)` (line 51) and
  `INIT_LIST_HEAD(&i->de_pin)` (line 52).
- `yolo_evict_inode()`: remove `WARN_ON_ONCE(!list_empty(&ii->de_list))`
  (line 70).
- `yolo_init_sbi()`: remove `INIT_LIST_HEAD(&sbi->pinned_dirs)` (line 163)
  and `spin_lock_init(&sbi->pinned_dirs_lock)` (line 164).

### 3. dentry.c — staging helpers

- `yolo_d_init()`: remove `INIT_LIST_HEAD(&info->de_node)` (line 33).
- `yolo_d_release()`: remove `WARN_ON_ONCE(!list_empty(&info->de_node))`
  (line 103).
- `yolo_stage_dentry()` (lines 131–141): remove `list_add(&di->de_node,
  &dii->de_list)`, `yolo_pin_dir_if_first()` call, and `dir` parameter.
  Becomes:
  ```c
  void yolo_stage_dentry(struct dentry *dentry, struct yolo_dstate dstate)
  {
      YOLO_D(dentry)->dstate = dstate;
      dget(dentry);    /* pin in dcache so it stays in d_children */
  }
  ```
  Update call sites: inode.c:46, inode.c:271, staging.c:157.
- `yolo_unstage_dentry()` (lines 149–155): remove `list_del_init(
  &di->de_node)`.  Becomes:
  ```c
  void yolo_unstage_dentry(struct yolo_dentry_info *di)
  {
      yolo_dstate_free(di->dstate);
      di->dstate = (struct yolo_dstate){0};
      dput(di->dentry);
  }
  ```
- `yolo_add_tombstone()` (lines 165–183): remove `list_add` to `de_list`
  (line 180) and `yolo_pin_dir_if_first()` call (line 181).  Drop `dir`
  parameter (was only used for those two calls).  Update call sites:
  inode.c:95, inode.c:190.
- `yolo_remove_tombstone()` (lines 190–195): remove `list_del_init` of
  `de_node` (line 192).  Drop `dir` parameter (unused after removal).
  Add `dstate = {0}` clear before `d_drop` so the dentry is no longer
  identified as staged if it lingers in `d_children`:
  ```c
  void yolo_remove_tombstone(struct dentry *tomb)
  {
      YOLO_D(tomb)->dstate = (struct yolo_dstate){0};
      d_drop(tomb);
      dput(tomb);
  }
  ```
  Update call sites: inode.c:107, inode.c:289.
- Remove `yolo_pin_dir_if_first()` function entirely (lines 116–125).

### 4. file.c — readdir

- `yolo_fill_base()` (lines 351–378): replace `de_list` iteration with
  `d_children` walk.  For each base entry name, look up in parent's
  `d_children` via `d_lookup()` and check `dstate.val != 0`:
  ```c
  child = d_lookup(dentry, &qstr);
  if (child) {
      overridden = !yolo_dstate_is_passthrough(YOLO_D(child)->dstate);
      dput(child);
      if (overridden) return true;   /* skip base entry */
  }
  ```
  This is O(1) per base entry via dcache hash, same as Plan 22's Phase 2
  design (no `d_children` scan needed here).

- `yolo_emit_dirents()` (lines 385–413): change parameter from
  `struct inode *dir` to `struct dentry *parent` (caller passes
  `file_dentry(file)`).  Replace `de_list` iteration with `d_children`
  walk.  `dir_emit()` copies to userspace and can page-fault, so we
  cannot hold `d_lock` across the call.  Use a pin-and-release pattern
  (like `dcache_readdir()` in libfs.c): acquire
  `spin_lock(&parent->d_lock)`, `dget_dlock(child)` to pin,
  `spin_unlock(&parent->d_lock)`, emit, `dput(child)`, re-acquire lock
  to advance.
  Filter: emit if `!yolo_dstate_is_passthrough(YOLO_D(child)->dstate)` and
  `!yolo_dstate_is_tombstone(YOLO_D(child)->dstate)`.

- `yolo_readdir()` fast-path (line 430): drop the
  `list_empty(&YOLO_I(file_inode(file))->de_list)` check entirely.
  Keep only `!sbi->staging || !sbi->inodes_dir.dentry`.  The slow path
  handles dirs with no staged entries efficiently: `yolo_emit_dirents`
  walks `d_children`, finds nothing staged, returns false; `yolo_fill_base`
  does `d_lookup` per base entry which misses or finds passthrough entries
  — O(1) each.

### 5. inode.c — create, delete, rename

- `yolo_create_staged()` (line 40): replace `!list_empty(&di->de_node)`
  with `!yolo_dstate_is_passthrough(di->dstate)`.

- `yolo_delete_entry()` (lines 85, 111): replace `!list_empty(&di->de_node)`
  with `!yolo_dstate_is_passthrough(di->dstate)`.

- `yolo_rename()` (lines 168, 177, 255, 261): replace all
  `!list_empty(&…->de_node)` checks with
  `!yolo_dstate_is_passthrough(…->dstate)`.  Remove `list_del_init(
  &old_di->de_node)` at line 261 (unstaging now handled by
  `yolo_unstage_dentry()` which clears dstate and calls dput).

### 6. staging.c — COW and bulk cleanup

- COW path (line 156): replace `list_empty(&di->de_node)` with
  `yolo_dstate_is_passthrough(di->dstate)`.

- `yolo_release_pinned_dirs()` (lines 192–210): rewrite as a lockless
  recursive dentry tree walk from `sb->s_root`.  This function is only
  called during restore (ioctl.c, line 702) and unmount (super.c,
  line 349) — both are exclusive contexts with no concurrent VFS
  operations (restore holds `staging_sem` write + checks
  `staging_fd_count == 0`; unmount has no active references).  The
  current implementation acquires `inode_lock` per directory, but this
  is unnecessary in these exclusive contexts and is dropped in the
  rewrite.

  Walk each directory's `d_children` with `hlist_for_each_entry_safe`:
  unstage any child with `dstate.val != 0`, and recurse into
  subdirectories.  `_safe` saves the next pointer before processing,
  so our own `dput()` (which may remove the current entry from
  `d_children`) does not corrupt the iteration.  No locks, no temp
  fields needed.

  ```c
  static void release_staged_children(struct dentry *parent)
  {
      struct dentry *child;
      struct hlist_node *tmp;

      hlist_for_each_entry_safe(child, tmp, &parent->d_children, d_sib) {
          if (!hlist_empty(&child->d_children))
              release_staged_children(child);
          if (YOLO_D(child) &&
              !yolo_dstate_is_passthrough(YOLO_D(child)->dstate))
              yolo_unstage_dentry(YOLO_D(child));
      }
  }

  void yolo_release_pinned_dirs(struct super_block *sb)
  {
      if (sb->s_root)
          release_staged_children(sb->s_root);
  }
  ```

  No new struct fields required.  Recursion depth is bounded by the
  directory tree depth of staged entries (in practice shallow; the
  kernel stack is 16 KiB which supports ~200+ frames of this size).

### 7. ioctl.c — restore path

- Remove `list_add(&YOLO_D(child)->de_node, &dii->de_list)` (line 627).
  The child is already pinned with `dget()` (or equivalent) by the
  restore code and is in `d_children` after `d_add()`.
- Remove `yolo_pin_dir_if_first(dii, sbi)` call (line 628).
- Update `yolo_release_pinned_dirs()` call (line 702) to pass `sb`
  instead of `sbi`.

### 8. super.c — unmount

- Update `yolo_kill_super()` call to `yolo_release_pinned_dirs()` (line 349)
  to pass `sb` instead of `sbi`.

### 9. Docs and tests

- Update `docs/staging.md` if it mentions `pinned_dirs`, `de_list`, or
  `de_node`.
- Run `make vm-test` to verify correctness.

## Notes

- `d_children` is a stable public VFS API (`hlist_head`) in Linux 6.8.
  Used by afs, coda, ceph, fsnotify, and libfs in-tree.
- `yolo_fill_base()` uses `d_lookup()` (O(1) dcache hash) which is
  strictly better than the current `de_list` linear scan (O(staged) per
  base entry).
- If readdir performance on dirs with many cached-but-unstaged children
  becomes a concern, a per-inode staged-children counter can be added
  later to restore a fast path.

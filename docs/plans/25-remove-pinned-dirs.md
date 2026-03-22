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
   and unmount (`agfs_release_pinned_dirs()`).  Can be replaced by walking
   `sb->s_inodes` and checking `d_children` for staged entries.

This removes five struct fields, one spinlock, one function
(`agfs_pin_dir_if_first()`), and simplifies six call-sites.

## Approach

- Replace `!list_empty(&di->de_node)` "is staged" test with
  `!agfs_dstate_is_passthrough(di->dstate)` (equivalently `di->dstate.val != 0`).
- Readdir emit (phase 1): walk `d_children` + `dstate.val != 0` filter.
- Readdir dedup (phase 2): use `d_lookup()` per base entry — O(1) dcache
  hash lookup, strictly better than the current O(staged) `de_list` scan.
- Bulk cleanup: replace `pinned_dirs` iteration with `sb->s_inodes` scan.
- Drop unused `dir` parameter from `agfs_stage_dentry()` and
  `agfs_remove_tombstone()`.
- Drop the readdir fast-path `de_list` emptiness check — the remaining
  `!sbi->staging || !sbi->inodes_dir.dentry` check handles the common
  case.  The slow path is already efficient for dirs with no staged entries.
- Locking: `d_children` is an `hlist_head` protected by `dentry->d_lock`.
  `libfs.c:dcache_readdir()` and `afs/dynroot.c` walk it under `i_rwsem`.
  AgFS readdir already holds `i_rwsem` (shared) via VFS `iterate_shared`,
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
| `agfs_sb_info` | `pinned_dirs` | Global list of dirs with staged children |
| `agfs_sb_info` | `pinned_dirs_lock` | Spinlock protecting above |
| `agfs_inode_info` | `de_list` | Per-dir list of staged children |
| `agfs_inode_info` | `de_pin` | Node in `sbi->pinned_dirs` |
| `agfs_dentry_info` | `de_node` | Node in parent's `de_list` |

## Functions removed

- `agfs_pin_dir_if_first()` (dentry.c) — no longer needed; nothing to pin to.

## Signature changes

- `agfs_stage_dentry()`: drop `dir` parameter (was only used for
  `list_add` and `agfs_pin_dir_if_first`).  Becomes a 2-liner: set
  dstate + `dget()`.
- `agfs_add_tombstone()`: drop `dir` parameter (was only used for
  `list_add` to `de_list` and `agfs_pin_dir_if_first`).
- `agfs_remove_tombstone()`: drop `dir` parameter (was only used for
  `list_del_init`).
- `agfs_emit_dirents()`: change from `struct inode *dir` to
  `struct dentry *parent` (needs dentry for `d_children` walk; caller
  passes `file_dentry(file)`).
- `agfs_release_pinned_dirs()`: change from `(struct agfs_sb_info *sbi)`
  to `(struct super_block *sb)`.

## Changes

### 1. agfs.h — struct changes and declarations

- Remove `de_list` from `agfs_inode_info` (line 356).
- Remove `de_pin` from `agfs_inode_info` (line 357).
- Remove `de_node` from `agfs_dentry_info` (line 368).
- Remove `pinned_dirs` from `agfs_sb_info` (line 335).
- Remove `pinned_dirs_lock` from `agfs_sb_info` (line 336).
- Remove `agfs_pin_dir_if_first()` declaration (lines 513–514).

### 2. super.c — init and eviction

- `agfs_alloc_inode()`: remove `INIT_LIST_HEAD(&i->de_list)` (line 51) and
  `INIT_LIST_HEAD(&i->de_pin)` (line 52).
- `agfs_evict_inode()`: remove `WARN_ON_ONCE(!list_empty(&ii->de_list))`
  (line 70).
- `agfs_init_sbi()`: remove `INIT_LIST_HEAD(&sbi->pinned_dirs)` (line 163)
  and `spin_lock_init(&sbi->pinned_dirs_lock)` (line 164).

### 3. dentry.c — staging helpers

- `agfs_d_init()`: remove `INIT_LIST_HEAD(&info->de_node)` (line 33).
- `agfs_d_release()`: remove `WARN_ON_ONCE(!list_empty(&info->de_node))`
  (line 103).
- `agfs_stage_dentry()` (lines 131–141): remove `list_add(&di->de_node,
  &dii->de_list)`, `agfs_pin_dir_if_first()` call, and `dir` parameter.
  Becomes:
  ```c
  void agfs_stage_dentry(struct dentry *dentry, struct agfs_dstate dstate)
  {
      AGFS_D(dentry)->dstate = dstate;
      dget(dentry);    /* pin in dcache so it stays in d_children */
  }
  ```
  Update call sites: inode.c:46, inode.c:244, staging.c:173.
- `agfs_unstage_dentry()` (lines 149–155): remove `list_del_init(
  &di->de_node)`.  Becomes:
  ```c
  void agfs_unstage_dentry(struct agfs_dentry_info *di)
  {
      agfs_dstate_free(di->dstate);
      di->dstate = (struct agfs_dstate){0};
      dput(di->dentry);
  }
  ```
- `agfs_add_tombstone()` (lines 165–183): remove `list_add` to `de_list`
  (line 180) and `agfs_pin_dir_if_first()` call (line 181).  Drop `dir`
  parameter (was only used for those two calls).  Update call sites:
  inode.c:107, inode.c:262.
- `agfs_remove_tombstone()` (lines 190–195): remove `list_del_init` of
  `de_node` (line 192).  Add `dstate = {0}` clear before `d_drop` so the
  dentry is no longer identified as staged if it lingers in `d_children`:
  ```c
  void agfs_remove_tombstone(struct dentry *tomb, struct inode *dir)
  {
      AGFS_D(tomb)->dstate = (struct agfs_dstate){0};
      d_drop(tomb);
      dput(tomb);
  }
  ```  Drop `dir` parameter (unused after removal).
  Update call sites: inode.c:107, inode.c:262.
- Remove `agfs_pin_dir_if_first()` function entirely (lines 116–125).

### 4. file.c — readdir

- `agfs_fill_base()` (lines 350–377): replace `de_list` iteration with
  `d_children` walk.  For each base entry name, look up in parent's
  `d_children` via `d_lookup()` and check `dstate.val != 0`:
  ```c
  child = d_lookup(dentry, &qstr);
  if (child) {
      overridden = !agfs_dstate_is_passthrough(AGFS_D(child)->dstate);
      dput(child);
      if (overridden) return true;   /* skip base entry */
  }
  ```
  This is O(1) per base entry via dcache hash, same as Plan 22's Phase 2
  design (no `d_children` scan needed here).

- `agfs_emit_dirents()` (lines 384–412): change parameter from
  `struct inode *dir` to `struct dentry *parent` (caller passes
  `file_dentry(file)`).  Replace `de_list` iteration with `d_children`
  walk.  `dir_emit()` copies to userspace and can page-fault, so we
  cannot hold `d_lock` across the call.  Use a pin-and-release pattern
  (like `dcache_readdir()` in libfs.c): acquire
  `spin_lock(&parent->d_lock)`, `dget_dlock(child)` to pin,
  `spin_unlock(&parent->d_lock)`, emit, `dput(child)`, re-acquire lock
  to advance.
  Filter: emit if `!agfs_dstate_is_passthrough(AGFS_D(child)->dstate)` and
  `!agfs_dstate_is_tombstone(AGFS_D(child)->dstate)`.

- `agfs_readdir()` fast-path (line 430): drop the
  `list_empty(&AGFS_I(file_inode(file))->de_list)` check entirely.
  Keep only `!sbi->staging || !sbi->inodes_dir.dentry`.  The slow path
  handles dirs with no staged entries efficiently: `agfs_emit_dirents`
  walks `d_children`, finds nothing staged, returns false; `agfs_fill_base`
  does `d_lookup` per base entry which misses or finds passthrough entries
  — O(1) each.

### 5. inode.c — create, delete, rename

- `agfs_create_staged()` (line 40): replace `!list_empty(&di->de_node)`
  with `!agfs_dstate_is_passthrough(di->dstate)`.

- `agfs_delete_entry()` (lines 85, 111): replace `!list_empty(&di->de_node)`
  with `!agfs_dstate_is_passthrough(di->dstate)`.

- `agfs_rename()` (lines 172, 181, 232, 238): replace all
  `!list_empty(&…->de_node)` checks with
  `!agfs_dstate_is_passthrough(…->dstate)`.  Remove `list_del_init(
  &old_di->de_node)` at line 238 (unstaging now handled by
  `agfs_unstage_dentry()` which clears dstate and calls dput).

### 6. staging.c — COW and bulk cleanup

- COW path (line 172): replace `list_empty(&di->de_node)` with
  `agfs_dstate_is_passthrough(di->dstate)`.

- `agfs_release_pinned_dirs()` (lines 208–227): rewrite to walk
  `sb->s_inodes` instead of `pinned_dirs`.  This function is only called
  during restore (ioctl.c, line 713) and unmount (super.c, line 349) —
  both are exclusive contexts with no concurrent VFS operations.
  Walking `sb->s_inodes` requires `sb->s_inode_list_lock` (a spinlock),
  but `agfs_unstage_dentry()` calls `dput()` which can sleep.  Use a
  two-phase approach: collect dentries-to-unstage under the lock, then
  unstage them after releasing it:
  ```c
  void agfs_release_pinned_dirs(struct super_block *sb)
  {
      struct inode *inode;
      LIST_HEAD(to_unstage);

      spin_lock(&sb->s_inode_list_lock);
      list_for_each_entry(inode, &sb->s_inodes, i_sb_list) {
          struct dentry *alias;

          if (!S_ISDIR(inode->i_mode))
              continue;

          hlist_for_each_entry(alias, &inode->i_dentry, d_u.d_alias) {
              struct dentry *child;

              hlist_for_each_entry(child, &alias->d_children, d_sib) {
                  struct agfs_dentry_info *di = AGFS_D(child);
                  if (!agfs_dstate_is_passthrough(di->dstate))
                      list_add(&di->de_unstage, &to_unstage);
              }
          }
      }
      spin_unlock(&sb->s_inode_list_lock);

      while (!list_empty(&to_unstage)) {
          struct agfs_dentry_info *di =
              list_first_entry(&to_unstage, struct agfs_dentry_info,
                               de_unstage);
          list_del_init(&di->de_unstage);
          agfs_unstage_dentry(di);
      }
  }
  ```

  This requires a temporary `de_unstage` list_head in
  `agfs_dentry_info`.  Alternatively, since these are exclusive contexts,
  use `list_for_each_entry` to build a simple array/count, or accept the
  simpler lockless walk given that unmount and restore already drain all
  VFS activity before running.

### 7. ioctl.c — restore path

- Remove `list_add(&AGFS_D(child)->de_node, &dii->de_list)` (line 638).
  The child is already pinned with `dget()` (or equivalent) by the
  restore code and is in `d_children` after `d_add()`.
- Remove `agfs_pin_dir_if_first(dii, sbi)` call (line 639).
- Update `agfs_release_pinned_dirs()` call (line 713) to pass `sb`
  instead of `sbi`.

### 8. super.c — unmount

- Update `agfs_kill_super()` call to `agfs_release_pinned_dirs()` (line 349)
  to pass `sb` instead of `sbi`.

### 9. Docs and tests

- Update `docs/staging.md` if it mentions `pinned_dirs`, `de_list`, or
  `de_node`.
- Run `make vm-test` to verify correctness.

## Notes

- `d_children` is a stable public VFS API (`hlist_head`) in Linux 6.8.
  Used by afs, coda, ceph, fsnotify, and libfs in-tree.
- `agfs_fill_base()` uses `d_lookup()` (O(1) dcache hash) which is
  strictly better than the current `de_list` linear scan (O(staged) per
  base entry).
- If readdir performance on dirs with many cached-but-unstaged children
  becomes a concern, a per-inode staged-children counter can be added
  later to restore a fast path.

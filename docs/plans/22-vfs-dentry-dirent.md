# 22 — Replace `agfs_dirent` hash table with VFS dentry state

## Problem

Directory overlay state is maintained in a custom per-directory hash table
(`de_buckets`: 1024 `hlist_head` buckets holding `agfs_dirent` entries).
Each `agfs_dirent` carries a 64-bit packed value (inode/link/tombstone) plus
an inline filename.  This duplicates the VFS dentry cache, which already
provides name hashing, O(1) lookup, and per-entry metadata via `d_fsdata`.
After a lookup, both a VFS dentry and an `agfs_dirent` exist for the same
entry — the dirent is purely redundant.

## Proposed approach

Eliminate `struct agfs_dirent` and the per-inode `de_buckets` hash table.
Store the packed overlay state (`struct agfs_dstate`) directly in
`struct agfs_dentry_info` (attached to every VFS dentry via `d_fsdata`).
Pin staged dentries with `dget()` to guarantee lifetime.

- **Point lookups** use the VFS dcache (O(1) via `lookup_fast()` /
  `d_lookup()`).
- **Iteration** (readdir Phase 1, bulk cleanup) uses a per-directory
  `de_list` — a linked list of pinned staged dentries, protected by
  `i_rwsem`.

No extra dentries are created during normal operations — VFS already
allocates the dentry during lookup/create/mkdir/rename.  The only new
dentry allocations are tombstones on unlink of base entries and entries
injected during restore (which front-load work that would happen on first
access anyway).

Target kernel: Linux 6.8+.

## Design

### agfs_dentry_info changes

```c
struct agfs_dentry_info {
    spinlock_t          lock;
    struct path         lower_path;
-   struct agfs_dirent  *dirent;      /* remove back-pointer */
+   struct agfs_dstate          packed;       /* overlay state: inode/link/tombstone */
+   struct list_head    de_node;      /* node in parent's de_list */
    enum agfs_perm      perm;
    struct list_head    rule_pin;
    struct dentry       *rule_dentry;
};
```

### agfs_inode_info changes

```c
struct agfs_inode_info {
    struct inode        *lower_inode;
    enum agfs_perm      cached_perm;
    u64                 perm_gen;
-   struct hlist_head   *de_buckets;
+   struct list_head    de_list;          /* pinned staged child dentries */
    struct list_head    de_pin;           /* node in sbi->pinned_dirs */
    struct inode        vfs_inode;
};
```

`de_pin` remains for enumerating directories with overlay state during
bulk cleanup (restore/unmount).  However, `igrab()` is no longer needed —
pinned child dentries hold a ref on `dentry->d_parent`, which transitively
keeps the parent inode alive through VFS refcounting.

### d_init callback

Add `d_init` to both `agfs_dops` and `agfs_dops_fast`.  This auto-initializes
`agfs_dentry_info` on every dentry at allocation time, eliminating the manual
`agfs_new_dentry_private_data()` call and ensuring tombstone dentries created
via `d_alloc()` have `d_fsdata` ready.

`d_init` must call `INIT_LIST_HEAD(&info->de_node)` so that
`list_empty(&de_node)` reliably distinguishes staged dentries (on `de_list`)
from unstaged ones.  This matters because `packed = 0` is the tombstone
encoding — without `de_node`, there is no way to tell "unstaged" from
"tombstone."  The rule: **a dentry is staged iff `!list_empty(&de_node)`**.

### Lookup

When VFS looks up a name, if a pinned staged dentry exists in the dcache,
`lookup_fast()` finds it directly — `->lookup()` is never called.
`d_revalidate` returns 1 (already implemented).

When `->lookup()` is called (no cached dentry), the name is not staged —
all staged entries are guaranteed to be pinned in the dcache.  The lookup
falls through directly to the base filesystem.  `agfs_lookup_staged()` is
eliminated entirely; `agfs_lookup()` simplifies to just the base path.

This also simplifies error handling: `d_init` already set up `d_fsdata`,
so lookup failure no longer needs to manually call
`agfs_free_dentry_private_data()` — VFS `dput()` triggers `d_release()`
which handles cleanup.

### Create / mkdir / symlink

VFS provides the dentry (from prior lookup).  If the dentry is a pinned
tombstone (on `de_list`), VFS still passes it to `->create()` as a negative
dentry — we reuse it directly, no new allocation.  After creating the staged
inode:

1. Check if dentry is staged: `!list_empty(&AGFS_D(dentry)->de_node)`.
   If so, it's a tombstone — inherit `in_base = true`.  Otherwise
   `in_base = false`.
2. Set `AGFS_D(dentry)->packed = agfs_dstate_inode(ino, gen, d_type, in_base)`.
   Use `WRITE_ONCE()` for the store.
3. If not already on `de_list`: `dget(dentry)` to pin, add to parent's
   `de_list`.  (Tombstone reuse: already pinned and on `de_list`, skip.)
4. Add parent directory to `sbi->pinned_dirs` (via `de_pin`) if this is the
   first entry in `de_list`.  No `igrab()` — the pinned child keeps the
   parent alive.

### Unlink / rmdir

Three cases based on current dentry state:

- **Staged + in_base=true** (modified base entry, on `de_list`):
  Remove old dentry from `de_list`, `dput()` old pin, `d_drop()` old dentry.
  Create tombstone: `d_alloc(parent, &name)` (gets `d_fsdata` via `d_init`),
  set `packed = tombstone` **before** `d_add()` (prevents RCU-walk race),
  `d_add(new, NULL)`, `dget(new)`, add to parent's `de_list`.

- **Staged + in_base=false** (staging-only entry, on `de_list`):
  Remove from parent's `de_list`, `dput()` the pin, `d_drop()`.
  Entry disappears entirely (same squash semantics as current
  tombstone + `!in_base` removal).

- **Not staged** (base-only entry, not on `de_list`):
  `d_drop()` old dentry.  Create tombstone: `d_alloc(parent, &name)`,
  set `packed = tombstone`, `d_add(new, NULL)`, `dget(new)`, add to
  parent's `de_list`.

VFS holds `i_rwsem` exclusive on the parent throughout, so no lookup
can race between `d_drop()` and `d_add()`.

### Rename

VFS provides old and new dentries with `i_rwsem` held on both parents.

1. Read `AGFS_D(old_dentry)->packed` for source state.
2. Check `AGFS_D(new_dentry)->packed` for `in_base` on destination.
3. Set `AGFS_D(new_dentry)->packed` with the new state (inode, link, or
   redirected link).  Pin new_dentry if not already pinned, add to new
   parent's `de_list`.
4. Tombstone or remove old_dentry (same logic as unlink).
5. Journal R/P record.

### COW / agfs_read_dirent

Currently follows `AGFS_D(dentry)->dirent->packed` under parent's `i_rwsem`.
With the new design, `packed` is directly in `AGFS_D(dentry)`.  Since
`struct agfs_dstate` is a u64 (naturally atomic on x86-64), reads use `READ_ONCE()`
and writes use `WRITE_ONCE()` — no lock needed to read own state:

```c
static struct agfs_dstate agfs_read_dirent(struct dentry *dentry)
{
    return (struct agfs_dstate){ .val = READ_ONCE(AGFS_D(dentry)->packed.val) };
}
```

The existing dentry_info spinlock remains for `lower_path` (two-pointer
struct requiring atomic swap), but `packed` reads are lockless.  This
eliminates lock contention on the COW fast path.

### Readdir

Two-phase merge stays the same, but both phases change:

**Phase 1 — staged entries:**  Iterate parent's `de_list` (our own
`list_head`, not VFS `d_children`).  Protected by `i_rwsem` (held by VFS
for `iterate_shared`), so `dir_emit()` (which may fault) is safe — no
`d_lock` spinlock needed.

```
for each child dentry in de_list (via de_node):
    packed = AGFS_D(child)->packed
    skip if tombstone
    dir_emit(child->d_name, d_type, ino from packed)
```

Resumption: same `off` / `ctx->pos` counter approach as today.

**Phase 2 — base entries:**  For each base entry, check if it's overridden
via `d_lookup(parent_dentry, &qstr)`.  O(1) dcache hash lookup.  If found
and the result is staged (`!list_empty(&AGFS_D(result)->de_node)`), skip the
base entry.  `dput()` the result after checking.

### Restore

1. `agfs_release_pinned_dirs()` changes:

   ```
   for each dir in pinned_dirs (via de_pin):
       for each child dentry in de_list (via de_node):
           list_del_init(de_node)
           dput(child)             // release our pin
       list_del_init(de_pin)
       // no iput() — child dput() cascades to parent via d_parent refs
   ```

2. `shrink_dcache_sb()` as before.

3. `agfs_restore_inject()`: for each entry in the serialized tree, use
   `d_alloc_name()` to create a dentry (`d_init` sets up `d_fsdata`),
   resolve the staged inode via `agfs_inode_path()` + `agfs_iget()`,
   set `packed`, `d_add(dentry, inode)`, `dget()` to pin, add to parent's
   `de_list`.  For tombstones: `d_add(dentry, NULL)`.

### Checkpoint

No change needed — checkpoint only bumps generation and writes journal.

## Structs / functions removed

- `struct agfs_dirent` (agfs.h)
- `AGFS_DE_SHIFT`, `AGFS_DE_BUCKETS` (agfs.h)
- `agfs_de_hash()` (staging.c)
- `agfs_find_dirent()` (staging.c)
- `agfs_add_dirent()` (staging.c)
- `agfs_del_dirent()` (staging.c)
- `agfs_ensure_de_buckets()` (staging.c)
- `agfs_free_de_buckets()` / `agfs_free_de_buckets_locked()` (staging.c)
- `agfs_new_dentry_private_data()` (dentry.c — replaced by `d_init`)
- `agfs_lookup_staged()` (lookup.c — dcache handles staged lookups)

## Structs / functions added or changed

- `agfs_dentry_info`: add `packed` field, add `de_node`, remove `dirent`
- `agfs_inode_info`: replace `de_buckets` with `de_list`
- `d_init` callback in `agfs_dops` / `agfs_dops_fast`: allocate
  `agfs_dentry_info`, `INIT_LIST_HEAD(&de_node)`, zero `packed`
- `d_release`: `WARN_ON_ONCE(!list_empty(&de_node))`, call
  `agfs_dstate_free(packed)` for link cleanup
- `agfs_pin_dir()`: trigger on first `de_list` entry; no `igrab()` (child
  dentry refs keep parent alive)
- `agfs_release_pinned_dirs()`: walk `de_list` per directory, `dput()` each
  child; no `iput()` (cascaded via VFS refcounting)
- `agfs_lookup()`: remove `agfs_lookup_staged()` call and `out_free` error
  path; base-only lookup
- `agfs_delete_entry()`: tombstone path creates negative dentry via
  `d_alloc()` + set packed + `d_add(NULL)` + `dget()`
- `agfs_read_dirent()`: lockless `READ_ONCE()` on `packed`
- `agfs_emit_dirents()`: iterate `de_list`
- `agfs_fill_base()`: use `d_lookup()` + `list_empty(&de_node)` for dedup

## Files affected

| File | Scope of change |
|------|-----------------|
| agfs.h | Struct definitions, remove dirent API declarations, add new fields |
| dentry.c | Add `d_init`, remove `agfs_new_dentry_private_data`, update `d_release` |
| staging.c | Remove hash table code, rewrite pin/release around `de_list` |
| lookup.c | Remove `agfs_lookup_staged()` entirely, simplify `agfs_lookup()` |
| inode.c | Rewrite create/delete/rename to set packed on dentry directly |
| file.c | Rewrite readdir Phase 1 + Phase 2 dedup, simplify `agfs_read_dirent` |
| ioctl.c | Rewrite restore injection to create/pin VFS dentries |
| super.c | Update `agfs_alloc_inode` init, keep eviction WARN |

## Resolved questions

1. **Staged vs unstaged disambiguation**: `packed = 0` is the tombstone
   encoding, which collides with the zero-initialized default.
   `list_empty(&de_node)` is the canonical "is staged" test — tombstones
   are always on `de_list`, unstaged dentries never are.

2. **Readdir Phase 2 dedup**: Use `d_lookup()` — O(1) dcache hash lookup.
   RCU fast path in common case.  No lock ordering issues (`i_rwsem` shared
   + dcache-internal locks is standard VFS ordering).  Check
   `!list_empty(&de_node)` on the result to distinguish staged from cached
   base entries.

3. **Restore performance**: `d_alloc_name()` replaces `kmalloc(agfs_dirent)`
   — comparable slab allocation cost.  Inode resolution
   (`agfs_inode_path()` + `agfs_iget()`) is front-loaded but is the same
   total work as lazy resolution on first access.

4. **Memory**: After lookup, the new design uses **less** memory (VFS dentry
   + `agfs_dentry_info` = ~256B) than the current design (VFS dentry +
   `agfs_dentry_info` + `agfs_dirent` = ~296B).  Before first access
   (restore-injected entries), the new design allocates a full dentry (~192B)
   vs an `agfs_dirent` (~40B), but this dentry would be created on first
   access anyway.

5. **Tombstone locking**: `d_alloc()` under `i_rwsem` exclusive is safe
   (`kmem_cache_alloc` can sleep under sleeping lock, no lock ordering
   conflict with dcache spinlocks).  `d_init` ensures `d_fsdata` is ready
   before `d_alloc()` returns.  Set `packed` before `d_add()` — dentry is
   not visible in dcache until `d_add()`, so no RCU-walk race.

6. **`igrab()` elimination**: Pinned child dentries hold a ref on
   `dentry->d_parent`, which holds a ref on the parent inode.  As long as
   any child is pinned, the parent inode (and its `de_list`) stays alive.
   `de_pin` remains as a list node in `sbi->pinned_dirs` for enumeration
   during bulk cleanup, but `igrab()`/`iput()` are no longer needed.

7. **Lockless `packed` reads**: `struct agfs_dstate` is a u64, naturally atomic on
   x86-64.  `READ_ONCE()` / `WRITE_ONCE()` suffice — the dentry_info
   spinlock is only needed for `lower_path` (two-pointer swap).  Eliminates
   lock contention on the COW fast path.

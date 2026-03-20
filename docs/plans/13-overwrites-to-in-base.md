# Plan 13: Rename `overwrites` → `in_base`

## Problem

The `overwrites` flag on `agfs_dirent` tracks whether a path existed in
the base layer. This determines which journal tag the kernel emits:
ADD vs MOD (staged content) and RDR vs REP (base-only renames). Comments
throughout the codebase describe this imprecisely — using "new path",
"existing path", or "existing content" without specifying that "existing"
means "in base".

Rename the flag to `in_base` and fix all comments to be precise.

## Design rationale

The `in_base` bit makes the journal **self-describing**: each record
carries enough information to resolve the full staging state without
filesystem I/O. `compact()` and `collapse()` are pure functions over
records — no stat calls, no dependency on the base filesystem.

The kernel records `in_base` at write time — the one moment it
definitively knows the answer. Downstream consumers never ask again:

- **Cancel**: `ADD+DEL → ∅` (nothing in base) vs `MOD+DEL → DEL` (base file must go)
- **Decompose**: `RDR+MOD → DEL+ADD` (dest empty in base) vs `REP+MOD → DEL+MOD` (dest in base)
- **Restore**: passes correct `in_base` to kernel so future records
  inherit the right value
- **Display**: "Added" vs "Modified" without stat calls

An alternative design could drop the bit entirely and check the
base filesystem at resolution time. We chose self-describing records
to keep resolution pure.

## COW fix

`agfs_do_cow()` (staging.c:367) hardcodes `.overwrites = true`. This is
correct for base files and redirects (they originate from base), but
wrong for **re-COW of staged-only files** after a checkpoint — the file
was never in base, yet we emit MOD instead of ADD.

### Fix

Change `agfs_do_cow()` to inherit `in_base` from the existing dirent.
The lookup must happen under `inode_lock` (which is already held for
the `agfs_add_dirent` call):

```c
de = (struct agfs_dirent){
    .ino = ino,
    .d_type = DT_REG,
    .gen = (u64)atomic64_read(&sbi->gen),
};
inode_lock(d_inode(dentry->d_parent));
old_de = agfs_find_dirent(d_inode(dentry->d_parent),
                          dentry->d_name.name,
                          dentry->d_name.len);
de.in_base = old_de ? old_de->in_base : true;
err = agfs_add_dirent(...);
inode_unlock(d_inode(dentry->d_parent));
```

If no dirent exists (first COW of a base file), default to `true`.
If a dirent exists (re-COW after checkpoint), inherit its `in_base`.

### Compaction impact

The fix introduces ADD+ADD sequences at the same path (create, checkpoint,
re-COW). The current compaction pipeline can't handle this:

- `merge_modifies` only merges MOD+MOD, not ADD+ADD.
- `cancel` only tracks the *latest* ADD/MOD per path, so `ADD(x,1), ADD(x,2),
  DEL(x)` orphans `ADD(x,1)` — it cancels the second ADD with DEL but forgets
  the first.

**Solution**: replace `merge_modifies` with a broader `deduplicate` pass
that handles ADD+ADD, ADD+MOD, MOD+ADD, and MOD+MOD. Move it *before* cancel so
duplicates are collapsed first:

```
Old order:  decompose → cancel → merge_modifies
New order:  decompose → deduplicate → cancel
```

The `deduplicate` pass keeps only the latest ADD/MOD per path, resetting
its tracking on DEL/RDR/REP (so it doesn't merge across deletes):

```
ADD(x,1), ADD(x,2)          → ADD(x,2)              (dedup)
ADD(x,1), ADD(x,2), DEL(x)  → ADD(x,2), DEL(x) → ∅   (dedup then cancel)
ADD(x,1), DEL(x), ADD(x,2)  → unchanged → ADD(x,2)  (dedup resets on DEL; cancel pairs ADD+DEL)
MOD(x,1), MOD(x,2), DEL(x)  → MOD(x,2), DEL(x) → DEL   (dedup then cancel)
```

## Changes

### kmod/ (kernel C)

| File | Change |
|------|--------|
| `agfs.h` | Rename `overwrites` field → `in_base` in both `agfs_ioc_restore_entry` (ioctl struct) and `agfs_dirent`. Update field comments to say "path exists in base layer". |
| `inode.c` | Rename local `overwrites` → `in_base`, `dst_overwrites` → `dst_in_base`. Update comments: "inherit overwrites" → "inherit in_base", "Check if destination has existing content" → "Check if destination exists in base". Reword line 156 comment (uses "overwrites" as a verb — clarify it refers to the dirent, not the flag). |
| `staging.c` | Rename all `.overwrites` field accesses → `.in_base`. Update comments. At COW site (line 367), **inherit `in_base` from existing dirent** instead of hardcoding `true` (see implementation above). |
| `ioctl.c` | Rename `.overwrites` → `.in_base` (line 469). |
| `journal.c` | Update format comments: "ADD (new file)" → "ADD (path not in base)", "MOD (existing file)" → "MOD (path exists in base)", "RDR (rename, new path)" → "RDR (dest not in base)", "REP (rename, existing path)" → "REP (dest exists in base)". |

### cli/ (Rust)

| File | Change |
|------|--------|
| `ioctl.rs` | Rename `pub overwrites: u8` → `pub in_base: u8`. |
| `journal/action.rs` | Rename `overwrites` parameter/variable → `in_base` in `collapse_rename()` and `insert_rename()`. |
| `journal/compact.rs` | Replace `merge_modifies` with `deduplicate` (handles ADD+ADD, ADD+MOD, MOD+ADD, MOD+MOD; resets on DEL/RDR/REP). Reorder passes: decompose → deduplicate → cancel. Update header rules and pass numbering. Add unit tests for ADD+ADD dedup, ADD+ADD+DEL cancel, and ADD+DEL+ADD (dedup resets on DEL). |
| `cmd/restore.rs` | Rename `overwrites` field in `RestoreItem` → `in_base`. Rename local variable. |
| `parse.rs` | Update format comments: "ADD (staged, new path)" → "ADD (path not in base)", "MOD (staged, existing path)" → "MOD (path exists in base)", "RDR (rename, new path)" → "RDR (dest not in base)", "REP (rename, existing path)" → "REP (dest exists in base)". |

### docs/

| File | Change |
|------|--------|
| `staging.md` | Rename ~44 occurrences of `overwrites` → `in_base`. Update the dirent field table, the "Tracking overwrites" section header and content, and all explanatory text to use "in base" rather than "existing content" or "had content". |

### tests/

| File | Change |
|------|--------|
| `tests/internals/test_overwrites.rs` | Rename file → `test_in_base.rs`. Update module-level comment and all doc comments. Rename `recow_of_staged_file_after_checkpoint_emits_modify` → `recow_of_staged_file_after_checkpoint_emits_add` and flip assertion from M to A. |
| `tests/internals/mod.rs` | `mod test_overwrites` → `mod test_in_base`. |
| `tests/internals/test_rename.rs` | Update 2 comments. |
| `tests/cli/test_restore.rs` | Update 3 comments mentioning `overwrites`. |
| `tests/fs/test_rename.rs` | Update 1 comment. |
| `tests/internals/test_write.rs` | `truncate_rewrite_overwrites_from_start` — NOT renamed (uses "overwrites" as a verb meaning "writes over bytes", not the flag). |

## Build & test

```bash
make vm-build
make vm-test
```

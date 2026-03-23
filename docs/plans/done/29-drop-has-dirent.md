# 29 — Drop `has_dirent` from Restore Wire Format

## Problem

The restore wire format has a `has_dirent:u8` byte per node that distinguishes
Passthrough dirs (`has_dirent=0`) from nodes with a real Dstate (`has_dirent=1`).
This is unnecessary: the dstate val already has a natural sentinel — `val == 0`
means Passthrough (matching the in-kernel representation where `val == 0` is
Passthrough). Dropping `has_dirent` makes the wire format 1:1 with the in-memory
tree and saves one byte per non-Passthrough node (Passthrough dirs grow by 7
bytes since they now write `val=0u64` instead of just `has_dirent=0`, but
they are uncommon in the wire format — empty ones and Passthrough files are
already filtered out).

## New Wire Format

```
DirTree  := child_count:le16  DirNode[child_count]
DirNode  := name_len:le16  name:u8[name_len]
            Dstate
            child_count:le16  DirNode[child_count]
Dstate   := val:le64                                    (0 = Passthrough)
            [base_len:le16  base_path:u8[base_len]  if BasePath]
```

File nodes always have `child_count=0`. The `File` vs `Dir` distinction is
implicit in dtype and child_count.

## Changes

### 1. Update docs (`docs/plans/done/19-restore-ioctl-tree-format.md`)

Update the wire format spec, state mapping table, and pseudocode to remove
`has_dirent`. Note that `val == 0` is now accepted (Passthrough) instead
of rejected.

### 2. CLI serialization (`cli/journal/tree.rs`)

- `serialize_dstate()`: handle `Dstate::Passthrough` by writing `0u64` instead
  of `unreachable!()`.
- `serialize_into()`: remove the `has_dirent` byte entirely. Collapse the
  `DirNode::Dir` match arm — delete the `if matches!(dstate, Dstate::Passthrough)`
  branch and unconditionally call `serialize_dstate(dstate, buf)` for all nodes
  (files and dirs alike), since `serialize_dstate()` now handles Passthrough.
- Update the wire format doc comment on `serialize()`.

### 3. Kernel parsing (`kmod/ioctl.c`)

- `agfs_restore_inject()`: remove `has_dirent` variable and its `read_u8` call.
  Always read `val:le64` after the name.
- If `val == 0`: skip dentry creation (Passthrough — no `d_alloc`/`d_add`),
  proceed directly to reading `child_count`.
- If `val != 0`: create dentry and `d_add` as before (existing code path).
- Replace the skip check `!has_dirent && child_count == 0` with
  `val == 0 && child_count == 0` (empty passthroughs still need skipping).

### 4. Update all serialization tests (`cli/journal/tree.rs`)

15 tests construct or assert on `has_dirent` bytes or use cursor offsets that
assume its presence. Update each to remove the `has_dirent` byte from expected
buffers and cursor offsets:

- `serialize_single_inode_file`
- `serialize_single_tombstone`
- `serialize_single_link`
- `serialize_nested_directories`
- `serialize_passthrough_dir`
- `serialize_children_sorted_by_name`
- `serialize_passthrough_dir_empty_subtree_omitted`
- `serialize_stale_intermediates_after_cancel`
- `serialize_partial_stale_intermediates`
- `serialize_inode_bits_correct`
- `serialize_link_bits_correct`
- `serialize_passthrough_file_omitted`
- `serialize_passthrough_dir_no_dirent` — rename to
  `serialize_passthrough_dir_val_zero` (the old name references the removed
  concept)
- `serialize_tombstone_dir_dtype`
- `serialize_tombstone_symlink_dtype`

Two additional tests call `serialize()` but only assert on node filtering
(expected output is `vec![0x00, 0x00]` = root child_count=0), so they should
not need changes — verify this during implementation:

- `serialize_after_roundtrip_rename_omits_passthrough`
- `roundtrip_rename_dir_preserves_subtree`

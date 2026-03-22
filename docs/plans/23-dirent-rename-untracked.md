# 23 — Dstate variant rename + Untracked variant

## Problem

The `Dstate` enum variant names are unclear, `Tombstone` lacks a `dtype` field,
`Dir(Option<Dstate>, DirTree)` uses an unnecessary `Option`, and there is no
representation for "this dentry follows the base filesystem."

## Changes

### 1. Rename variants and fields

| Old                        | New                        |
|----------------------------|----------------------------|
| `Dstate::Inode { ino, .. }`| `Dstate::StagedInode { ino, .. }` |
| `Dstate::Link { base_path, .. }` | `Dstate::BasePath { src, .. }` |
| `Dstate::Tombstone`        | `Dstate::Tombstone { dtype }` |

### 2. Add `Dstate::Untracked` variant

New variant representing "follow the base state" (equivalent to a Link to
itself).  No fields — the dtype is implicit from the `DirNode` kind.

### 3. Remove `Option` from `DirNode::Dir`

`Dir(Option<Dstate>, DirTree)` → `Dir(Dstate, DirTree)`.

Intermediate directories (previously `Dir(None, ...)`) become
`Dir(Dstate::Untracked, ...)`.

### 4. Visibility rules

- `for_each()` / `into_dirents()`: emit `Untracked` for `File` nodes, skip for
  `Dir` nodes (intermediate dirs stay invisible, matching current behavior).
- `len()`: exclude `Untracked` entries.
- `diff.rs` (`print_change`): skip `Untracked` — it represents no user-visible
  change.

### 5. Serialization

Updated to match the kernel's new packed format:

- `Tombstone { dtype }`: serialize as `(dtype << 61) | (1 << 60)` — dtype in
  bits [62:61], in_base=1 at bit 60, ino=0.
- `Untracked`: serialize as `packed = 0` (all zeros).
- `Untracked` in `Dir` node: serialize the node with `has_dirent=0` but still
  serialize its subtree (pass-through to children).
- `Untracked` in `File` node: skip the node entirely (no change to
  communicate).

### 6. Behavioral changes

- Roundtrip rename collapse (a→tmp→a): instead of removing the node, insert
  `File(Dstate::Untracked)` (or `Dir(Dstate::Untracked, ...)` for dirs).
  This makes the "no net change" state explicit and visible in `into_dirents()`.
- `Tombstone.dtype()` now returns the real dtype instead of always `DType::File`.

## Files to modify

1. `cli/journal/tree.rs` — enum definition, impl, DirNode, all tree logic, all
   tests.
2. `cli/cmd/diff.rs` — update match arms; skip Untracked in print_change.
3. `cli/cmd/restore.rs` — update Dstate patterns in tests.
4. `tests/internals/test_consistency.rs` — update Dstate patterns.
5. `tests/internals/test_mkdir.rs` — update Dstate patterns.
6. `tests/internals/helpers.rs` — update Dstate patterns.
7. `tests/fs/test_rename.rs` — update Dstate patterns.
8. `docs/` — update any docs referencing Dstate variants.

## Deferred

None — kernel and userspace are both updated.

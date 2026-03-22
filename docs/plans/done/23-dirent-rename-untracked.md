# 23 — Dstate variant rename + Passthrough variant

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

### 2. Add `Dstate::Passthrough` variant

New variant representing "follow the base state" (equivalent to a Link to
itself).  No fields — the dtype is implicit from the `DirNode` kind.

### 3. Remove `Option` from `DirNode::Dir`

`Dir(Option<Dstate>, DirTree)` → `Dir(Dstate, DirTree)`.

Intermediate directories (previously `Dir(None, ...)`) become
`Dir(Dstate::Passthrough, ...)`.

### 4. Visibility rules

- `for_each()` / `into_dirents()`: emit `Passthrough` for `File` nodes, skip for
  `Dir` nodes (intermediate dirs stay invisible, matching current behavior).
- `len()`: exclude `Passthrough` entries.
- `diff.rs` (`print_change`): skip `Passthrough` — it represents no user-visible
  change.

### 5. Serialization

Updated to match the kernel's new packed format:

- `Tombstone { dtype }`: serialize as `(dtype << 61) | (1 << 60)` — dtype in
  bits [62:61], in_base=1 at bit 60, ino=0.
- `Passthrough`: serialize as `packed = 0` (all zeros).
- `Passthrough` in `Dir` node: serialize the node with `has_dirent=0` but still
  serialize its subtree (pass-through to children).
- `Passthrough` in `File` node: skip the node entirely (no change to
  communicate).

### 6. Behavioral changes

- Roundtrip rename collapse (a→tmp→a): instead of removing the node, insert
  `File(Dstate::Passthrough)` (or `Dir(Dstate::Passthrough, ...)` for dirs).
  This makes the "no net change" state explicit and visible in `into_dirents()`.
- `Tombstone.dtype()` now returns the real dtype instead of always `DType::File`.

## Files to modify

1. `cli/journal/tree.rs` — enum definition, impl, DirNode, all tree logic, all
   tests.
2. `cli/cmd/diff.rs` — update match arms; skip Passthrough in print_change.
3. `cli/cmd/restore.rs` — update Dstate patterns in tests.
4. `tests/internals/test_consistency.rs` — update Dstate patterns.
5. `tests/internals/test_mkdir.rs` — update Dstate patterns.
6. `tests/internals/helpers.rs` — update Dstate patterns.
7. `tests/fs/test_rename.rs` — update Dstate patterns.
8. `docs/` — update any docs referencing Dstate variants.

## Deferred

None — kernel and userspace are both updated.

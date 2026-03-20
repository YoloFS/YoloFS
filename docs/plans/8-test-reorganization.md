# Test Reorganization: Colocate by Operation

## Problem

The E2E tests are split into `tests/fs/` (black-box) and `tests/internals/` (white-box), but the boundary is blurry:

- `fs/` tests frequently call `s.cli(&["status"])`, `s.cli(&["commit"])`, `s.cli(&["abort"])` and inspect `s.base_path()` — mixing black-box with CLI and lifecycle verification.
- For every operation (create, write, delete, rename, symlink, mkdir), there are two parallel test files that test the same action at different abstraction levels, with no clear guidance on which assertions belong where.
- Adding a new operation requires remembering to add tests in multiple directories.

## Approach

Replace `tests/fs/` and `tests/internals/` with a single `tests/ops/` directory. Each per-operation file merges its `fs/` and `internals/` counterparts, organized by section headers:

```
// ── Filesystem ──       (std::fs through the mount)
// ── Journal ──          (raw journal records)
// ── Inode Store ──      (inode content and properties)
// ── Lifecycle ──        (commit/abort/checkpoint effects on this op)
```

Cross-cutting internals tests (commit_abort, checkpoint, restore, resolution, in_base, ownership, layout) move directly to `ops/` since they're still E2E tests that go through the mount.

`tests/cli/` and `tests/perm/` are **unchanged** — they have clean boundaries already.

## New Structure

```
tests/
  e2e.rs                    # harness: mod helpers; mod cli; mod ops; mod perm;
  helpers.rs                # AgfsSession (unchanged)
  ops/
    mod.rs
    helpers.rs              # journal/inode introspection (was internals/helpers.rs)
    # ── Per-operation (merged from fs/ + internals/) ──
    test_create.rs          # fs/test_create.rs + internals/test_create.rs
    test_write.rs           # fs/test_write.rs + internals/test_write.rs
    test_delete.rs          # fs/test_delete.rs + internals/test_delete.rs
    test_rename.rs          # fs/test_rename.rs + internals/test_rename.rs
    test_symlink.rs         # fs/test_symlink.rs + internals/test_symlink.rs
    test_mkdir.rs           # fs/test_mkdir.rs + internals/test_mkdir.rs
    # ── FS-only (moved from fs/) ──
    test_read.rs
    test_readdir.rs
    test_rmdir.rs
    test_concurrent.rs
    test_vfs.rs             # was fs/test_inode.rs (statfs, seek, getattr)
    # ── Cross-cutting internals (moved from internals/) ──
    test_commit_abort.rs
    test_checkpoint.rs
    test_restore.rs
    test_resolution.rs      # compound journal operation sequences
    test_in_base.rs         # A vs M record tagging
    test_ownership.rs       # inode ownership
    test_layout.rs          # inode store structure
  cli/                      # unchanged
  perm/                     # unchanged
```

## File Merge Details

For each of the 6 paired operations, the merged file follows this structure:

1. **Imports** — union of both files' imports (`crate::helpers::AgfsSession` + `super::helpers::{changes, ino_for, ...}`)
2. **`// ── Filesystem ──`** — tests from `fs/test_X.rs` that only use `std::fs` through the mount
3. **`// ── Journal ──`** — tests from `internals/test_X.rs` that check `Record` variants
4. **`// ── Inode Store ──`** — tests from `internals/test_X.rs` that inspect inode files
5. **`// ── Lifecycle ──`** — tests from `fs/test_X.rs` that call `s.cli(&["commit"])` / `s.cli(&["abort"])` / `s.cli(&["status"])`

## Renames

- `fs/test_inode.rs` → `ops/test_vfs.rs` (better name: it tests VFS behavior like statfs/seek/getattr, not inodes)

## Notes

- `test_ownership.rs` and `test_layout.rs` have overlapping ownership assertions (`staged_inode_owned_by_caller` appears in both). During the merge, deduplicate: keep the more comprehensive `test_ownership.rs` versions and remove the duplicate from `test_layout.rs`.
- All test function names are preserved — no renames — to keep `git blame` useful.
- Test count must be identical before and after (minus deduplicated ownership test).

## Todos

1. Create `tests/ops/mod.rs` and `tests/ops/helpers.rs`
2. Merge per-operation pairs (create, write, delete, rename, symlink, mkdir)
3. Move fs-only files (read, readdir, rmdir, concurrent, inode→vfs)
4. Move internals-only files (commit_abort, checkpoint, restore, resolution, in_base, ownership, layout)
5. Deduplicate ownership test between layout and ownership
6. Update `tests/e2e.rs` harness (`mod fs; mod internals;` → `mod ops;`)
7. Delete `tests/fs/` and `tests/internals/` directories
8. Update docs with new test taxonomy
9. Run tests to verify

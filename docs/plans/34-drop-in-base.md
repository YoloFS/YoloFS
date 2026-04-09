# 34 — Drop `in_base`

## Problem

`in_base` tracks per-dentry whether a path position had content in the base
filesystem. It is used to decide tombstone-vs-cancel on delete, select journal
tags (A/M, R/P), and classify entries for `yolo status`/`yolo diff`.

This tracking is fragile for directory renames: the kernel cannot efficiently
update `in_base` on all cached children when a directory moves, leaving stale
values that can cause wrong tombstone/cancel decisions. Removing `in_base`
eliminates this fragile invariant.

**Key insight**: the kernel can always place a tombstone on delete. A spurious
tombstone (hiding nothing in base) is harmless — lookup returns ENOENT and
readdir skips it either way. The only cost is a pinned dentry that could have
been freed (cleaned up on commit/reset). Userspace can derive the add/modify
distinction by querying the base filesystem.

**Commit safety**: `commit.rs` replays raw journal actions, not the tree. Its
delete branch checks `symlink_metadata()` before removing, so a `D` record
for a path that never existed in base is silently skipped. Spurious tombstones
do not affect commit correctness.

## Approach

- **Kernel**: remove `in_base` from `yolo_dentry_info`. Always tombstone on
  delete. Always use a single journal tag for create (`A`) and rename (`R`).
  Merge A/M → `A`, R/P → `R`. Simplify the restore wire format (drop the
  `in_base` byte).
- **Userspace**: remove `in_base` from `Dentry`. Merge `Add`/`Modify` →
  `Add`, `Rename`/`Replace` → `Rename` in journal parsing. The tree builder
  no longer tracks `in_base`. The diff/status command derives the add/modify
  distinction by checking the base filesystem.
- **Docs**: update staging.md and architecture.md to reflect the simplified
  state model.

## Changes

### 1. Docs — update before implementation

- `docs/staging.md`: remove `in_base` from the dentry state table (lines
  103–110), dentry state section (126–158), wire format (160–165), journal
  tag table (563–575), tracking section (597–662). Simplify: two-field model
  (`target`, `pinned`), single create tag `A`, single rename tag `R`, always
  tombstone on delete.
- `docs/architecture.md`: remove `in_base` from staging state description
  (line 79). Update dentry state references.

### 2. Kernel — `kmod/`

#### 2a. `yolofs.h` — drop field and update setter signature
- Remove `in_base` from `struct yolo_dentry_info`.
- Change `yolo_dentry_set` signature: drop `in_base` parameter. The function
  always pins unless `target == YOLO_TARGET_NONE && !tombstone`. Actually
  simpler: any staged entry is pinned; the only unpinned state is ground
  state (`NONE`, not a tombstone). Introduce a boolean `tombstone` parameter
  or use a separate `yolo_dentry_set_tombstone` helper if clearer.

#### 2b. `dentry.c` — simplify `yolo_dentry_set`
- Remove `in_base` storage and the `should_pin` logic that depends on it.
- New pinning rule: pin if `target != YOLO_TARGET_NONE` **or** if this is a
  tombstone. Ground state (`NONE`, not tombstone) unpins.
- `yolo_dentry_reset` remains the same — sets to ground state.

#### 2c. `lookup.c` — remove `in_base = true` on base hit
- Remove `YOLO_D(dentry)->in_base = true` (line 140). Not needed — lookup
  doesn't need to mark base-ness anymore.

#### 2d. `inode.c` — simplify create, delete, rename

**Create** (`yolo_create_staged`):
- Remove `in_base = YOLO_D(dentry)->pinned` logic.
- Always use `yolo_journal_add()`. Remove `yolo_journal_modify()`.

**Delete** (`yolo_delete_entry`):
- Remove the `if (YOLO_D(dentry)->in_base)` branch. Always create a
  tombstone (allocate negative dentry, set to tombstone state).
- Journal record is always `D` (unchanged).

**Rename** (`yolo_rename`):
- Remove `dst_in_base = YOLO_D(new_dentry)->in_base`.
- Always create a tombstone at the old name (remove the
  `if (YOLO_D(old_dentry)->in_base)` guard).
- Always use `yolo_journal_rename()`. Remove `yolo_journal_replace()`.
- Update the moved dentry without `in_base`: just set target.

#### 2e. `journal.c` — remove `yolo_journal_modify` and `yolo_journal_replace`
- Delete `yolo_journal_modify()`. All creates use `yolo_journal_add()` (`A`).
- Delete `yolo_journal_replace()`. All renames use `yolo_journal_rename()`
  (`R`).
- Remove declarations from `yolofs.h`.

#### 2f. `dir.c` — simplify tombstone check
- The tombstone check `target == NONE && in_base` becomes just
  `target == NONE && pinned` (or use a dedicated tombstone flag/state).

#### 2g. `ioctl.c` — simplify restore wire format
- Remove `in_base` byte from the restore tree parser. The wire format
  becomes: `tag:u8 [payload]` (no `in_base:u8` between tag and payload).
- `restore_add_dentry` drops `in_base` parameter. Tombstones are identified
  by `target == YOLO_TARGET_NONE`.

#### 2h. `staging.c`
- Update `yolo_dentry_set` call (line ~149) to drop `in_base` argument.
- `yolo_do_cow` (line ~127–177) currently calls `yolo_journal_modify()` and
  passes `in_base: true` to `yolo_dentry_set`. Switch to
  `yolo_journal_add()` and drop the `in_base` argument. COW is semantically
  a create (staged content replaces base content), so `A` is the correct
  tag once the kernel no longer distinguishes add/modify.

### 3. Userspace — `user/`

#### 3a. `user/journal/parse.rs` — merge tags
- Remove `Action::Modify` and `Action::Replace`.
- Parse `M` as `Action::Add`, `P` as `Action::Rename` (backwards compat with
  old journals, though not strictly needed if we don't support mixed-version).
- Or simply remove the `M` and `P` match arms if old journals won't be read.

#### 3b. `user/journal/dentry.rs` — remove `in_base` field
- Remove `in_base` from `struct Dentry`.
- Remove `in_base` from `passthrough()`.
- Update constructors / pattern matches.

#### 3c. `user/journal/tree.rs` — simplify tree builder
- Remove all `in_base` tracking in `apply_*` methods.
- `apply_add` / `apply_modify` → single `apply_add` (no base distinction).
- `apply_rename` / `apply_replace` → single `apply_rename` (always
  tombstone at source).
- Delete always tombstones (no cancel logic).
- Remove `in_base` from tree serialization (restore buffer). Wire format
  becomes `tag:u8 [payload]`.

#### 3d. `user/cmd/diff.rs` — derive classification from base FS
- Instead of checking `in_base`, use `utils::to_base_path(rel_path)` (which
  resolves to the host root, e.g. `/` + rel_path) and check whether the
  base path exists. `diff.rs` already has a `read_base()` helper that calls
  `to_base_path`; extend this pattern to an existence check.
- Full classification:
  - `Target::Inode(_)` + base path exists → **modified**
  - `Target::Inode(_)` + base path absent → **added**
  - `Target::None` + base path exists → **deleted**
  - `Target::None` + base path absent → **skip** (spurious tombstone for a
    staged-only file that was deleted; net no-op)
- This replaces the previous `(target, in_base)` match.

#### 3e. `user/cmd/restore.rs` — update serialization
- Drop `in_base` byte from the restore tree buffer sent to the kernel.

### 4. Tests

- **Unit tests** (`user/journal/tree.rs` tests): update all `in_base`
  assertions. Tests that assert `in_base: true/false` on `Dentry` need to be
  updated since the field no longer exists.
- **Diff tests** (`user/cmd/diff.rs`): update to reflect new classification
  logic (base FS check instead of `in_base`).
- **Restore tests** (`user/cmd/restore.rs`): update wire format expectations.
- **Integration tests**:
  - `tests/internals/test_cancel.rs` — the cancel concept goes away; delete
    always tombstones. Update or replace with tests verifying tombstone
    behavior for staged-only files.
  - `tests/internals/test_consistency.rs` — remove `in_base` assertions.
  - `tests/fs/test_rename.rs` — remove `in_base` assertions.
- **New tests**: add a test for directory rename + child delete to verify
  the tombstone is correct (the case that motivated this change).

## Order of Implementation

1. Docs (staging.md, architecture.md)
2. Kernel changes (2a–2h) — build with `make kmod`
3. Userspace changes (3a–3e) — build with `make user`
4. Test updates (4) — run with `make vm-test`

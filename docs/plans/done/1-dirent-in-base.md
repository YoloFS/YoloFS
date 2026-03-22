# Plan: Add `in_base` to `agfs_dirent` and `P` journal tag

## Problem

The `base` field on `agfs_dirent` is overloaded. It serves as both:
1. **Redirect source path** — for redirect dirents (`ino == REDIRECT`)
2. **In-base indicator** — `NULL` (not in base) vs `AGFS_BASE_PRESENT` (in base)

For staged and deleted dirents this works because `base` is free (no
redirect path). For redirect dirents, `base` always holds a non-NULL
redirect path, so `agfs_de_in_base()` returns true even when the
destination name was never in base. This caused a bug: rename-overwrite
chain collapse lost track of overwritten base files.

## Approach

Two changes that work together:

1. **Kernel dirent**: Add a dedicated `bool in_base` field to `struct
   agfs_dirent`. Stop using `AGFS_BASE_PRESENT` as a sentinel. Let `base`
   mean only "redirect source path" (non-NULL for redirects, NULL
   otherwise). This gives the kernel correct `dst_in_base` for subsequent
   renames at the same path.

2. **Journal `P` tag**: Add a new journal record `P` (replace-redirect)
   that parallels the `A`/`M` distinction for staged files:

   | Staged | Redirect | `in_base` |
   |--------|----------|-----------|
   | `A` (add) | `R` (redirect) | false — new path |
   | `M` (modify) | `P` (replace) | true — overwrites base file |

   `P` has the same fields as `R`: `P\0<dir>\0<name>\0<dtype>\0<base>\n`.
   The resolver gets `in_base` directly from the record type — no need to
   infer it from a preceding `D(dst)` record. Each segment is
   self-describing. `D` does not need an in-base variant — the resolver
   always infers it from prior state at the path.

3. **Resolver**: Replace `is_new`/`dst_in_base` with a single `in_base`
   field on `Stage` and `Redirect` actions, mirroring the kernel dirent.
   `Delete` stays plain (always represents a base-file deletion; staged-only
   files cancel before a Delete is created).

## Changes

### Kernel (`kmod/`)

1. **`agfs.h`** — `struct agfs_dirent`:
   - Add `bool in_base` field.
   - Remove `AGFS_BASE_PRESENT` sentinel.
   - `agfs_de_in_base()`: return `de->in_base` instead of `de->base != NULL`.
   - `agfs_de_base_free()`: drop sentinel check (just `kfree` or NULL).
   - `agfs_de_base_dup()`: drop sentinel check (just `kstrdup` or NULL).

2. **`staging.c`** — `agfs_add_dirent()`:
   - Update-in-place (existing dirent): for deletes, inherit
     `old_de->in_base`. For non-deletes, set `old_de->in_base =
     de->in_base`.
   - New dirent: for deletes without prior dirent, set `in_base = true`
     (file was only in base). Otherwise copy from template.
   - `base` handling: copy `de->base` as redirect path (no sentinel).
   - COW path (`agfs_do_cow` dirent): set `in_base = true` (COW always
     modifies a file that existed), `base = NULL` (staged, not redirect).

3. **`inode.c`** — `agfs_create_staged()`:
   - Set `de.in_base = in_base` (from deleted-dirent check).
   - Set `de.base = NULL` (staged files never have a redirect path).

4. **`inode.c`** — `agfs_rename()`:
   - Staged source: `de.in_base = dst_in_base`, `de.base = NULL`.
   - Redirect source: `de.in_base = dst_in_base`, `de.base = redirect_path`.
   - Journal: emit `P` instead of `R` when
     `dst_in_base && !agfs_ino_is_staged(ino)`. Revert the `D(dst)`
     workaround added earlier.

5. **`journal.c`** — add `agfs_journal_replace()`:
   - Same as `agfs_journal_redirect()` but emits tag `P` instead of `R`.

6. **`ioctl.c`** — restore entry injection:
   - Add `__u8 in_base` to `struct agfs_ioc_restore_entry`
     (use one of the padding bytes).
   - Set `de.in_base` from entry when injecting dirents.

### CLI (`cli/`)

7. **`cli/journal/types.rs`** — Record enum:
   - Add `Record::Replace` variant with same fields as `Redirect`
     (path, dtype, base). Parallels `Added`/`Modified` for staged files:
     ```rust
     Record::Added    { path, dtype, ino }    // A — staged, new
     Record::Modified { path, dtype, ino }    // M — staged, existing
     Record::Redirect { path, dtype, base }   // R — redirect, new
     Record::Replace  { path, dtype, base }   // P — redirect, existing
     ```

8. **`cli/journal/parse.rs`** — parser:
   - Parse `P` tag same as `R` but produce `Record::Replace`.
   - Update the format comment to document the `P` tag.

9. **`cli/journal/resolve.rs`** — Action enum + Resolver:
   - Rename `Action::Rename` to `Action::Redirect` and replace
     `dst_in_base` with `in_base`:
     ```rust
     Stage    { ino: u64, dtype: DType, in_base: bool }
     Redirect { origin: String, dtype: DType, in_base: bool, ino: Option<u64> }
     Delete
     ```
   - Remove `is_new` (it was `!in_base`).
   - `process_stage()`: receive `in_base` instead of `is_new`.
     - `A` record → `in_base = false`.
     - `M` record → `in_base = true`.
     - When prior state is Delete: always `in_base = true` (Delete
       means the path existed).
     - When prior state is Redirect: inherit `redirect.in_base`.
   - Delete handler:
     - `Stage { in_base: false }` → cancels (staged-only).
     - `Stage { in_base: true }` → `Delete`.
     - `Redirect { in_base, .. }` → `Delete(origin)` + if `in_base`:
       `Delete(path)`.
   - Redirect handler:
     - `Record::Redirect` → `process_redirect(path, dtype, base, in_base: false)`.
     - `Record::Replace` → `process_redirect(path, dtype, base, in_base: true)`.
     - Parallels how `Added`/`Modified` call `process_stage` with
       `in_base` false/true.
     - When destination has prior `Redirect { origin, .. }` →
       `Delete(origin)`. New Redirect gets its `in_base` from the
       record, not inherited from the prior Redirect.
     - When destination has prior `Stage` → silently replaced. The
       REP/RDR record carries the correct `in_base` from the kernel
       (which read the dirent's `in_base` before the rename).
     - When destination has prior `Delete` → silently replaced. Same
       reasoning — the REP/RDR record is authoritative.
   - `Change::Renamed` — add `in_base: bool`:
     ```rust
     Renamed { from: String, to: String, dtype: DType, in_base: bool }
     ```
     Propagated from `Redirect.in_base` in `emit_action()`. Display
     code ignores it; restore uses it for correct ioctl entries.
   - `emit_action()`: `Stage { in_base: false }` → Added,
     `Stage { in_base: true }` → Modified.
     `Redirect` → `Renamed { in_base }` (propagated).

10. **`cli/restore.rs`** — RestoreItem:
    - Add `in_base: bool` to `RestoreItem`.
    - Populate from resolved Changes:
      - Added → `in_base = false`
      - Modified → `in_base = true`
      - Deleted → `in_base = true`
      - Renamed source (deleted entry) → `in_base = true`
      - Renamed dest (redirect entry) → `in_base` from `Change::Renamed`
    - Pass through to ioctl entry.

11. **`cli/ioctl.rs`** — `AgfsIocRestoreEntry`:
    - Add `in_base: u8` field (using one padding byte to match the kernel
      struct change).

### Revert earlier workaround

12. **Revert `D(dst)` changes** from this session:
    - `kmod/inode.c`: remove the `D(dst)` emission block added before the
      existing journal records in `agfs_rename()`. Replace with `P` tag
      emission (item 4).
    - `cli/journal/resolve.rs`: remove `dst_in_base` field from
      `Action::Rename`, remove `D(dst)` inference in Redirect handler,
      remove `dst_in_base` propagation in Delete handler. Superseded by
      `in_base` on `Action::Redirect` (item 9).

### Tests

13. **`cli/journal/resolve.rs`** unit tests:

    **Update existing tests** to use `in_base` / `P` tag semantics:
    - All tests constructing `Action::Rename` → use `Action::Redirect`.
    - All tests using `is_new` → use `in_base`.

    **Rename-overwrite tests** (update journal data):
    - `rename_overwrite_base_then_move`: use `P` tag instead of
      `D(dst) + R`. Assert `Renamed(b→c) + Deleted(a)`.
    - `rename_overwrite_base_then_delete`: use `P` tag instead of
      `D(dst) + R`. Assert `Deleted(a) + Deleted(b)`.

    **New tests to add:**
    - `staged_only_delete_cancels`: `A(x) + D(x)` → empty (no Change).
    - `redirect_new_path_then_delete`: `D(a) + R(b, /a) + D(b)` →
      `Deleted(a)` only (b was never in base, no `Deleted(b)`).
    - `replace_redirect_then_delete`: `D(a) + P(b, /a) + D(b)` →
      `Deleted(a) + Deleted(b)` (b was in base, P carries `in_base`).
    - `replace_redirect_simple`: `D(a) + P(b, /a)` →
      `Renamed(a→b)`. Commit `fs::rename` overwrites base/b.
    - `redirect_overwrites_prior_stage`: `A(b, ino=1) + D(a) + R(b, /a)`
      → `Renamed(a→b)` (prior staged add at b silently replaced).
    - `redirect_overwrites_prior_stage_in_base`:
      `M(b, ino=1) + D(a) + P(b, /a)` → `Renamed(a→b)` (prior modify
      at b silently replaced; P carries `in_base=true`).
    - `chain_rename_through_base_path`:
      `D(b) + P(a, /b) + D(a) + R(c, /b)` →
      `Deleted(a) + Renamed(b→c)`.
    - `restore_renamed_in_base`: verify `Change::Renamed` carries
      `in_base` correctly for restore item generation.

### Docs

14. **`docs/staging.md`**:
    - Update journal format table with `P` tag.
    - Update `agfs_dirent` struct documentation with `in_base` field.
    - Update edge cases to reference `in_base` instead of `base` pointer
      semantics.
    - Remove references to `AGFS_BASE_PRESENT` sentinel.
    - Update rename pseudocode to emit `P` instead of `D(dst) + R`.
    - Document the `A`/`M` ↔ `R`/`P` symmetry in the journal format
      section.

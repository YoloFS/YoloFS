# Plan: Replace Literal Journal Bytes with Direct Record Construction

## Problem

Unit tests in `cli/journal/resolve.rs` construct journal data by writing
raw byte slices:

```rust
data.extend_from_slice(b"D\0/dir\0c\n");
data.extend_from_slice(b"P\0/dir\0a\0f\0/dir/b\n");
```

There are ~250 of these across ~92 tests. This is:
- **Fragile** — if the journal format changes (new field, different
  separator), every test must be updated by hand.
- **Hard to read** — the NUL-separated binary format obscures what each
  test is actually doing.
- **Error-prone** — easy to get field order wrong or forget a field.
- **Unnecessarily slow** — tests serialize to bytes, write to disk, read
  from disk, then parse back into `Record` values, even though
  `resolve()` and `resolve_segments()` accept `Vec<Record>` directly.

## Approach

Construct `Vec<Record>` directly instead of round-tripping through bytes
and disk I/O. No builder struct, no byte-formatting helpers — just use
the existing `Record` enum.

Before:
```rust
let dir = setup_test_dir();
let mut data = Vec::new();
data.extend_from_slice(b"A\0/nonexistent_test_12345\0new.txt\0f\01\n");
fs::write(dir.path().join("journal"), &data).unwrap();
fs::write(dir.path().join("inodes/1"), "content").unwrap();
let changes = resolve(read(dir.path())).unwrap();
```

After:
```rust
let changes = resolve(vec![
    Record::Added { path: "/nonexistent_test_12345/new.txt".into(), dtype: Some(DType::File), ino: 1 },
]).unwrap();
```

This eliminates `setup_test_dir()`, `fs::write`, tempfiles, and the
test-only `read()` helper entirely. The newer tests (e.g.
`resolve_segments_after_reachable`) already use this pattern.

## Todos

| ID | Task | Files |
|----|------|-------|
| migrate-resolve | Convert all ~92 tests from byte-slices + disk I/O to direct `Vec<Record>` construction | `cli/journal/resolve.rs` |

## Notes

- `resolve()` and `resolve_segments()` take `Vec<Record>` — no disk
  needed. The inode file writes in current tests are vestigial (no test
  reads them back).
- `setup_test_dir()`, the test-only `read()` helper, and `use std::fs`
  can be removed once all tests are migrated.
- Parse tests in `parse.rs` still need raw bytes — that's correct since
  they test the parser itself.
- The newer tests at the bottom of the file already follow this pattern
  and serve as the reference style.

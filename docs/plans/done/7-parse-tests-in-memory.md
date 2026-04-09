# Plan: Extract In-Memory Parser for Journal Tests

## Problem

Unit tests in `cli/journal/parse.rs` test pure parsing logic but go
through disk I/O: they create a `tempfile::TempDir`, `fs::write` raw
bytes into a `journal` file, then call `read()` which does `fs::read`
and parses. This is unnecessary — the parsing step is purely in-memory
and doesn't need a filesystem.

This makes the tests:
- **Slower than necessary** — disk round-trip for every test.
- **Noisy** — `setup_test_dir()`, `fs::create_dir_all`, `fs::write`
  boilerplate obscures what's being tested.
- **Over-coupled** — every parse test depends on the `read()` function's
  file-path convention (`yolo_dir.join("journal")`), when they only care
  about byte→Record conversion.

## Approach

Extract the byte-parsing loop from `read()` into a standalone
`parse(data: &[u8]) -> Result<Journal>` function. Then `read()` becomes
a thin wrapper:

```rust
pub fn read(yolo_dir: &Path) -> Result<Journal> {
    let journal_path = yolo_dir.join("journal");
    if !journal_path.exists() {
        return Ok(Journal { records: Vec::new() });
    }
    let data = fs::read(&journal_path).context("reading journal file")?;
    parse(&data)
}

pub fn parse(data: &[u8]) -> Result<Journal> {
    // ... existing parsing logic moved here ...
}
```

Most tests then call `parse()` directly with in-memory byte slices:

Before:
```rust
let dir = setup_test_dir();
fs::write(dir.path().join("journal"), b"A\0\0a\0f\01\n").unwrap();
let journal = read(dir.path()).unwrap();
```

After:
```rust
let journal = parse(b"A\0\0a\0f\01\n").unwrap();
```

## Todos

| ID | Task | Files |
|----|------|-------|
| extract-parse | Extract `parse(data: &[u8]) -> Result<Journal>` from `read()`, keeping `read()` as a thin wrapper | `cli/journal/parse.rs` |
| migrate-parse-tests | Convert all parsing tests to call `parse()` directly; remove `setup_test_dir()`, `fs::write`, and `tempfile` usage from those tests | `cli/journal/parse.rs` |
| keep-io-tests | Keep or add a minimal test for `read()` itself (e.g. `read_empty` for missing file) and `inode_path_format` — these genuinely need a path | `cli/journal/parse.rs` |

## Notes

- `parse()` should be `pub` so `resolve.rs` tests or other modules can
  use it if needed, but it can start as `pub(super)` if we prefer
  tighter visibility.
- `dtype_to_libc` test doesn't touch the journal at all — it stays
  unchanged.
- `setup_test_dir()` can remain for the few tests that still need it
  (`read_empty`, `inode_path_format`), or we can inline the setup since
  there will be only 1–2 such tests.
- This pairs well with plan 5: after both plans, no parse or resolve
  test does unnecessary disk I/O.

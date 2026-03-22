# 27 — Remove to_dirents, use for_each everywhere

## Problem

`to_dirents()` collects the entire tree into a `Vec<(String, Dstate)>` which is
unnecessary now that `DirTree::get(path)` and `for_each()` exist.

## Approach

### 1. `helpers::dirents()` → `helpers::tree()`

Change return type from `Vec<(String, Dstate)>` to `DirTree`. Callers that used
the Vec now use `for_each` or `get`.

### 2. `helpers::ino_for(dstates, suffix)` → `helpers::ino_for(tree, suffix)`

Rewrite to take `&DirTree` and use `for_each` internally (suffix matching via
`ends_with`, same logic, no Vec).

### 3. Update all e2e test call sites

- `let ch = dstates(&s); let ino = ino_for(&ch, ...)` → `let tree = tree(&s); let ino = ino_for(&tree, ...)`
- `test_consistency.rs` iteration: `for (path, dstate) in &dstates(s)` → `tree.for_each(|path, dstate| { ... })`

### 4. Update tree.rs unit tests

Replace remaining `tree.to_dirents()` in debug format args with direct `tree`
debug output (DirTree derives Debug). Or just use `{tree:?}`.

### 5. Remove `to_dirents()`

Delete the method from DirTree.

### 6. `tests/fs/test_rename.rs` and `tests/internals/test_restore.rs`

These also call `to_dirents()` — convert them to use `for_each` or `get`.

## Files affected

- `cli/journal/tree.rs` — remove `to_dirents()`, update unit test debug msgs
- `tests/internals/helpers.rs` — `dstates()` → `tree()`, rewrite `ino_for`
- `tests/internals/test_*.rs` — update imports and call sites
- `tests/fs/test_rename.rs` — convert to `for_each`

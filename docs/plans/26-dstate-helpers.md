# 26 — Add DirTree helpers, rename dirent→dstate

## Problem

Tests use `to_dirents()` to flatten the entire tree into a `Vec<(String, Dstate)>`
just to assert on individual entries. This is wasteful and obscures intent. Also,
the naming "dirent" is outdated — the type is `Dstate`.

## Approach

### 1. Add `DirTree::get(path) -> Option<&Dstate>`

Walk the tree by splitting path components. O(depth) lookup instead of O(n) flatten.

### 2. Replace test usage of `to_dirents()`

All test patterns and their replacements:

| Pattern | Replacement |
|---|---|
| `dirents.len()` / `is_empty()` | `tree.len()` / `tree.is_empty()` (already exist) |
| `dirents[0]` (when len==1) | `tree.get(path)` |
| `dirents.iter().any(\|(p,c)\| p == X && matches!(c, ...))` | `matches!(tree.get(X), Some(...))` |
| `dirents.iter().map(\|(p,_)\| p).collect()` (paths list) | keep `to_dirents` for the 1-2 tests that need full path lists, or add `tree.paths()` |

### 3. Rename dirent→dstate in internal names

- `visit_dirents` → `visit_dstates`
- `set_dirent` → `set_dstate`
- `serialize_dirent` → `serialize_dstate`
- `DirNode::leaf(dirent:)` param → `dstate`
- Doc comments: "dirent" → "dstate" where referring to `Dstate` values
- `for_each` callback param names
- `tests/internals/helpers.rs`: `dirents()` → `dstates()`, `ino_for(dirents,)` param
- Variable names in tests (`dirent` → `dstate`)
- Keep "dirent" in comments that refer to the kernel dirent table concept (line 4)
- Keep `has_dirent` in serialize wire format (it's a protocol field name)

### 4. Update `tests/internals/` helpers

- `helpers::dirents()` → `helpers::dstates()` returns `Vec<(String, Dstate)>` (still needed for e2e tests that iterate all entries)
- `helpers::ino_for()` param rename
- Update all call sites in `test_mkdir.rs`, `test_write.rs`, `test_consistency.rs`

### 5. Remove `to_dirents` if no longer needed

If all unit tests migrate to `get()` + `len()`, remove `to_dirents` (or keep it if e2e helpers still use it).

## Files affected

- `cli/journal/tree.rs` — add `get()`, rename internals, rewrite unit tests
- `cli/cmd/restore.rs` — rewrite unit tests
- `cli/cmd/diff.rs` — rename `dirent` params/vars
- `tests/internals/helpers.rs` — rename `dirents()` → `dstates()`
- `tests/internals/test_mkdir.rs` — update call sites
- `tests/internals/test_write.rs` — update call sites
- `tests/internals/test_consistency.rs` — update call sites

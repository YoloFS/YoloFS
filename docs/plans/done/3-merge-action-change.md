# Plan: Merge Action into Change

## Problem

`Action` and `Change` are now structurally identical (same 5 variants,
same fields). `emit_action` is a trivial 1:1 mapping. Two types for the
same thing.

## Approach

Eliminate `Action`. Use `Change` as the BTreeMap value directly. Remove
`path`/`from`/`to` from `Change` — return `Vec<(String, Change)>` from
`resolve()` where the String is the primary path.

### Change becomes path-free:

```rust
pub enum Change {
    Added { ino: u64, dtype: DType },
    Modified { ino: u64, dtype: DType },
    Deleted,
    Renamed { from: String, dtype: DType },
    Replaced { from: String, dtype: DType },
}
```

`resolve()` returns `Vec<(String, Change)>` — the String is the
destination path (the BTreeMap key):
- Added/Modified/Deleted: `(path, Change::...)`
- Renamed/Replaced: `(to, Change::Renamed { from, dtype })`

### Consumers to update

1. **cli/commit.rs** — `apply_changes` matches on `(path, change)`
2. **cli/diff.rs** — diff display + state_map
3. **cli/restore.rs** — `changes_to_items`
4. **cli/journal/resolve.rs** — delete Action, delete emit_action,
   update resolver to insert Change directly, update into_changes
5. **tests/fs/test_rename.rs**
6. **tests/internals/helpers.rs**
7. **tests/internals/test_mkdir.rs**

~225 references total. Mechanical but large.

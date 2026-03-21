# 2 — Owned `apply(Action)` for DirTree

## Problem

`DirTree::apply(&Action)` borrows Actions, forcing `.to_string()` on every
path component inserted into the HashMap. Since callers almost always own
the Actions (parsed from the journal), we can move path strings directly
into the tree, eliminating one heap allocation per record for the leaf name
and up to two more for rename operations.

## Approach

Change `apply` to take `Action` by value. Internal methods (`set_dirent`,
`apply_delete`, `apply_rename`) take owned `String` paths. A new
`walk_to_parent_owned` splits the leaf name from the path in-place via
`String::drain` (zero allocation). A new `walk_to_parent_lookup` avoids
allocation in `detach` (lookup-only, no `entry`).

Callers that own their segments (abort, mount, restore) use a new
`build_owned`. The borrowing `build_from_segments` remains for diff.rs,
cloning each Action (same cost as today's `.to_string()`).

## Changes

### tree.rs
- Remove `walk_to_parent` (borrowed, creating intermediates).
- Add `walk_to_parent_owned(String) → (&mut DirTree, String)` — uses `drain`.
- Add `walk_to_parent_lookup(&str) → (&mut DirTree, &str)` — uses `get_mut`.
- `set_dirent(String, Dirent)` — owned path, moves leaf into HashMap.
- `apply_delete(String, Option<DType>)` — owned path.
- `apply_rename(String, String, DType, bool)` — owned dst/src.
- `apply(Action)` — by value, destructures and moves.
- `apply_all(impl IntoIterator<Item = Action>)` — consuming iterator.
- `build_from_segments` — clones Actions from borrowed segments.
- `build_owned(impl IntoIterator<Item = Segment>)` — consuming, zero-clone.
- `detach` uses `walk_to_parent_lookup`.
- Test helper `build()` uses `build_owned`.

### journal.rs
- `into_live_segments(self) → impl Iterator<Item = Segment>`.
- `into_live_segments_at(self, u64) → impl Iterator<Item = Segment>`.

### Callers
- abort.rs, mount.rs, restore.rs: use `into_live_segments` + `build_owned`.
- diff.rs: unchanged (borrowing path).

## Allocation savings per Action type

| Type       | Before (borrowing) | After (owned) |
|------------|---------------------|---------------|
| Add/Modify | 1 (leaf name)       | 0             |
| Delete     | 0–1 (tombstone)     | 0             |
| Rename     | 3 (link + src leaf + dst leaf) | 1 (link clone) |

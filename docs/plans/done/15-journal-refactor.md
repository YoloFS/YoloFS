# Plan: Journal Module Refactor

Split `Record` into `Action` + `Marker`, replace `SegmentedJournal` with
`Journal` (borrowing interface, precomputed liveness), reorganize files,
and document the gen_id invariant.

## Background

The journal module has a clean pipeline but several design issues:

1. **Mixed types.** `Record` mixes data mutations (A/M/D/R/P) with control
   markers (K/T), forcing dead match arms in consumers. `Markers` stores
   `Vec<Record>` but only works if all entries are K/T — nothing enforces this.

2. **Consuming interface.** `SegmentedJournal`'s filter methods (`live()`,
   `live_prefix()`, `live_slice()`) take `self` by value, destroying the
   struct. But `DirTree::build()` immediately borrows the result. This forces
   unnecessary moves and prevents reuse.

3. **Redundant allocation.** Every filter method collects into a new
   `Vec<Segment>`. Callers iterate that vec once and discard it. The
   intermediate vec is pure waste.

4. **Repeated liveness computation.** `alive_segments()` is recomputed on
   every call. The alive mask is immutable after construction.

5. **Scattered file layout.** `liveness.rs` has orphan impls for two different
   types. `segment.rs` is a single small struct. Responsibilities are split
   across files without clear boundaries.

## File Structure

Before (7 files):
```
journal/
  mod.rs       — re-exports only
  types.rs     — Record, DType, Segment, INO_REDIRECT
  parse.rs     — read(), parse()
  markers.rs   — Markers (lookup, range)
  segment.rs   — SegmentedJournal (struct + new)
  liveness.rs  — impl Markers {alive_*} + impl SegmentedJournal {live*}
  tree.rs      — DirTree, Dirent, DirNode
```

After (6 files, one responsibility each):
```
journal/
  mod.rs       — re-exports only
  types.rs     — Action, Marker, Record, DType, Segment, INO_REDIRECT
  parse.rs     — parse()  (read becomes pub(super))
  markers.rs   — Markers (lookup + range + alive_segments + checkpoint_at)
  journal.rs   — Journal (struct + new + read + live_segments_*)
  tree.rs      — DirTree, Dirent, DirNode
```

Deleted: `segment.rs` (replaced by `journal.rs`),
`liveness.rs` (Markers impls → `markers.rs`, Journal methods → `journal.rs`).

Principle: no orphan impls — every type's methods live in the same file as
its definition.

## Changes

### 1. Split `Record` into `Action` + `Marker`

In `types.rs`:

```rust
/// A data mutation applied to the dir tree (A/M/D/R/P).
enum Action {
    Add     { path: String, dtype: Option<DType>, ino: u64 },
    Modify  { path: String, dtype: Option<DType>, ino: u64 },
    Delete  { path: String, dtype: Option<DType> },
    Rename  { src: String, dst: String, dtype: Option<DType> },
    Replace { src: String, dst: String, dtype: Option<DType> },
}

/// A control marker (K/T).
enum Marker {
    Checkpoint { gen_id: u64, name: String },
    Restore    { gen_id: u64, target_gen: u64 },
}

/// A parsed journal record (interleaved actions and markers).
enum Record {
    Action(Action),
    Marker(Marker),
}
```

Variant names use imperative form — they are actions applied to the tree,
not descriptions of state. `Redirect` is renamed to `Rename` for clarity.

Update downstream:
- `Segment.records: Vec<Action>` — no dead arms.
- `Markers(Vec<Marker>)` — type-safe.
- `DirTree::apply(&mut self, action: &Action)` — no K/T match needed.
- `parse::parse()` returns `Vec<Record>` as before, but each variant wraps
  the inner type.
- All cmd/ files that pattern-match on Record variants: diff, timeline,
  audit, restore, mount, commit, abort.
- `audit.rs` specifically: `record_matches_path()` takes `&Action` (not
  `&Record`), `format_record()` splits into `format_action()` +
  `format_marker()` (or two match arms on separate types).

### 2. Replace `SegmentedJournal` with `Journal`

#### Type definition (in `journal.rs`)

```rust
pub struct Journal {
    pub segments: Vec<Segment>,
    pub markers: Markers,
    alive: Vec<bool>,  // precomputed in new(), one entry per segment
}
```

`alive` is computed once at construction via `markers.alive_segments()`.

#### Constructor

```rust
impl Journal {
    pub fn new(records: Vec<Record>) -> Self;
    pub fn read(agfs_dir: &Path) -> Result<Self>;
}
```

`read()` calls `parse::read()` internally.

#### parse.rs visibility

Both `parse::read()` and `parse::parse()` become `pub(super)` — internal to
the journal module. External callers use `Journal::read()` or
`Journal::new()` with hand-built records. `parse::parse()` is still
accessible from `#[cfg(test)]` modules within `journal/`.

#### Borrowing filter methods

All filter methods take `&self` and return iterators over `&Segment`.
No moves, no collects, no intermediate allocations.

```rust
impl Journal {
    /// All live segments (filtered by precomputed alive mask).
    pub fn live_segments(&self) -> impl Iterator<Item = &Segment>;

    /// Live segments up to a checkpoint (by gen_id).
    /// Computes its own alive mask scoped to the prefix, because
    /// restore records after the prefix boundary do not affect it.
    pub fn live_segments_at(&self, gen_id: u64) -> impl Iterator<Item = &Segment>;

    /// Convenience: resolve name then call live_segments_at.
    pub fn live_segments_at_name(&self, name: &str) -> Result<impl Iterator<Item = &Segment>>;

    /// Slice by --at/--from/--to.
    pub fn live_segments_slice(
        &self,
        at: Option<&str>,
        from: Option<&str>,
        to: Option<&str>,
    ) -> Result<impl Iterator<Item = &Segment>>;

    /// Whether segment at this index is alive (for audit/timeline display).
    pub fn is_alive(&self, segment_index: usize) -> bool;
}
```

#### Markers updates

- `Markers(Vec<Marker>)` — type-safe inner type.
- `get()` and `iter()` return `&Marker` instead of `&Record`.
- `closing_checkpoint()` renamed to `checkpoint_at()`.
- `alive_segments()` and `alive_segments_range()` move from `liveness.rs`
  into `markers.rs`.
- All existing methods stay: `find_checkpoint()`, `find_checkpoint_by_name()`,
  `find_checkpoint_by_gen_id()`, `segment_range()`, `len()`, `is_empty()`.
  Their internal matching changes from `Record::Checkpoint` to
  `Marker::Checkpoint`.

#### Caller migration

Commands that use `Journal`: diff, timeline, audit, restore, mount, commit,
abort. All change from:
```rust
let sj = SegmentedJournal::new(journal::read(&agfs)?);
```
to:
```rust
let journal = Journal::read(&agfs)?;
```

And from consuming methods to borrowing:
```rust
// Before: let live = sj.live(); DirTree::build(&live);
// After:
let tree = DirTree::build(journal.live_segments());
```

`DirTree::build` changes signature to accept an iterator:
```rust
pub fn build<'a>(segments: impl IntoIterator<Item = &'a Segment>) -> Self;
```

`audit` and `timeline` access `journal.segments`, `journal.markers`, and
`journal.is_alive(i)` directly.

`diff` changes from destructuring `(Segment, Option<(u64, String)>)` tuples
to iterating segments and calling `markers.checkpoint_at(segment_index)` for
header display.

All pattern matching on `Record` variants in cmd/ files updates:
- `Record::Added` → `Action::Add`
- `Record::Modified` → `Action::Modify`
- `Record::Deleted` → `Action::Delete`
- `Record::Redirect` → `Action::Rename`
- `Record::Replace` → `Action::Replace`
- `Record::Checkpoint` → `Marker::Checkpoint`
- `Record::Restore` → `Marker::Restore`

#### mod.rs re-exports

```rust
pub use journal::Journal;
pub use markers::Markers;
pub use tree::{DirTree, Dirent};
pub use types::*;  // Action, Marker, Record, DType, Segment, INO_REDIRECT
```

No parse re-exports — `parse` is `pub(super)` only.

#### Test migration

- `segment.rs` segmentation tests → `journal.rs` (they test `Journal::new`)
- `segment.rs` markers tests → `markers.rs`
- `liveness.rs` alive computation tests → `markers.rs`
- `liveness.rs` live_segments/slice/prefix tests → `journal.rs`
- All test Record constructors update to new enum wrapping
  (e.g. `Record::Added { .. }` → `Record::Action(Action::Add { .. })`)

### 3. Document the gen_id invariant

The kernel increments `sbi->gen` via `atomic64_inc_return()` on every K and T
record. Gen_id values are strictly sequential: marker\[i\] has gen_id = i + 1.
The `Markers` type relies on this for O(1) lookup. Add a doc-comment on
`Markers::find_checkpoint_by_gen_id` stating this invariant, and add a note
in `staging.md` after the record type table (around line 534).

### 4. Update documentation

Update `docs/staging.md` and `docs/architecture.md`:
- `SegmentedJournal` → `Journal`
- `live()` → `live_segments()`
- `live_slice()` → `live_segments_slice()`
- `live_prefix()` / `live_prefix_gen()` → `live_segments_at()` / `live_segments_at_name()`
- `closing_checkpoint()` → `checkpoint_at()`
- Remove references to deleted files `segment.rs` and `liveness.rs`

### 5. Path pre-splitting — not now

Path components are split inside `DirTree::walk_to_parent()` on every call.
This is fine because:
- Each record hits `walk_to_parent` once — no redundant splitting.
- The split is zero-allocation (just index arithmetic on &str).
- Callers like audit/diff need the full path string for display, so
  pre-splitting at parse time would require reconstructing strings.

If profiling shows tree construction as a bottleneck, the optimization would
be to split once inside `DirTree::apply()` and pass `&[&str]` components to
the internal helpers. But this is not worth the complexity today.

## Order of Implementation

1. Document gen_id invariant (independent, no code change).
2. Split `Record` → `Action` + `Marker` + `Record` (pure refactor, update all callers).
3. Replace `SegmentedJournal` with `Journal` + reorganize files (create `journal.rs`,
   delete `segment.rs` and `liveness.rs`, merge liveness into `markers.rs`,
   update `mod.rs` re-exports, migrate tests).
4. Update docs (`staging.md`, `architecture.md`).

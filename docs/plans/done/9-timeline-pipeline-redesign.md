# Plan: Timeline Pipeline Redesign

## Problem

The current `Timeline` struct bundles three concerns into one:
- The original flat record list (`all_records`)
- A per-record reachability mask (`reachable_mask`)
- Structured segments (`Vec<Segment>`)

This design has issues:
- The mask and segments redundantly encode reachability.
- Segments split only at K boundaries, so a restore inside a segment
  creates mixed reachability — the segment abstraction breaks down.
- The `Segment` struct carries a `reachable: bool` flag even though
  reachability is a journal-level property, not a segment property.

## Approach

Redesign the journal processing as a clean four-level data pipeline
where each level is a distinct transformation.

### Data Structures

**`Segment`** — a checkpoint and the changes that followed it:
```rust
struct Segment {
    from: Option<Checkpoint>,
    records: Vec<Record>,   // ADD/MOD/DEL/RDR/REP only
}
```

`from` is `None` for the 0-th segment (records before the first
checkpoint). All other segments have `Some(checkpoint)`.

No `to`, no `reachable`. Pure structural grouping. Resolution produces
a `Changeset` per segment — no wrapper struct needed since the caller
already has the segment's `from` checkpoint for context.

**`Markers`** — the CKP/RST skeleton of the journal:
```rust
enum Marker {
    Checkpoint { pos: usize, checkpoint: Checkpoint },
    Restore { pos: usize, target_gen: u64 },
}

struct Markers(Vec<Marker>);
```

Lightweight metadata extracted alongside segments. Answers:
- Which segments are alive (from RST records + target CKP positions).
- Where is checkpoint X (for `find_checkpoint`).

### Pipeline

```
Level 0    RawJournal                         parsed from disk
              │
              │ segment (split at K and S boundaries)
              ▼
Level 1    SegmentedJournal                   all segments + CKP/RST skeleton
              │
              │ filter by Markers
              ▼
Level 2    LiveSegments                       live segments only
              │
              │ resolve (per-segment or flat)
              ▼
Level 3    Changeset                          resolved changes
```

Segments are split at both K and S boundaries. This guarantees every
segment has uniform reachability — no restore can appear inside a segment.
A segment following an S boundary inherits `from = restore target checkpoint`.

### Caller Workflows

| Caller | Consumes |
|--------|----------|
| journal_cmd | SegmentedJournal (iterate all segments + Markers, dim dead) |
| timeline_cmd | SegmentedJournal Markers only (display CKP/RST events, dim dead) |
| commit | LiveSegments → resolve → Changeset flat |
| restore | SegmentedJournal Markers (find checkpoint) → prefix → live filter → resolve → Changeset flat |
| diff/status | LiveSegments (optionally sliced via Markers) → resolve each → Changeset per-segment |



### Key Design Decisions

- **Segments split at S boundaries too.** This eliminates mixed
  reachability. The segment after an S boundary has
  `from = restore target`, which is semantically correct: new work
  builds on the restored checkpoint state.

- **Reachability is a transformation, not stored state.** The `Markers`
  struct carries the CKP/RST skeleton. Filtering live segments from all
  segments IS the reachability computation. No flags, no masks.

- **`Segment` has no `to` field.** The closing checkpoint is the next
  segment's `from` — derivable from context.

- **`Segment.from` is `Option<Checkpoint>`.** The 0-th segment (records
  before the first checkpoint) has `from = None`. All subsequent
  segments have `Some`. This preserves pre-first-checkpoint records
  instead of silently dropping them.

- **`Segment` has no `reachable` field.** Liveness is determined by
  `Markers`, not by the segment itself.

- **`resolve` takes `Vec<Record>` from a segment.** Per-segment
  resolution is just calling `resolve` on each segment's records.
  No `ResolvedSegment` wrapper — the caller pairs the result with the
  segment's `from` checkpoint. Flat resolution concatenates all
  segment records first, producing a single `Changeset`.

## Todos

| ID | Task | Files |
|----|------|-------|
| markers-struct | Define `Marker`, `Markers` struct with `is_segment_alive()` and `find_checkpoint()` methods | `cli/journal/timeline.rs` |
| segment-split | Rewrite segmentation to split at both K and S boundaries, producing `SegmentedJournal` | `cli/journal/timeline.rs` |
| live-filter | Implement `live()` filter: `SegmentedJournal → LiveSegments` | `cli/journal/timeline.rs` |
| remove-resolve-segments | Remove `resolve_segments` and `ResolvedSegment`; callers resolve each segment directly | `cli/journal/resolve.rs` |
| migrate-callers | Update all CLI commands to use the new pipeline; remove journal resolution from abort/mount entirely (they only used it for change counting in prompts — not worth the cost) | `cli/{journal_cmd,timeline_cmd,diff,commit,restore,abort,mount}.rs` |
| remove-old | Remove `Timeline` struct, `reachable_mask`, old flat functions, old `Segment` struct | `cli/journal/timeline.rs` |
| update-docs | Update `docs/staging.md` and `docs/architecture.md` to reflect new pipeline | `docs/` |
| update-tests | Update tests for new API; add tests for S-boundary splitting | `cli/journal/timeline.rs`, `tests/internals/` |

## Notes

- The common case (no restores) produces segments split only at K
  boundaries. S-splitting adds segments only when restores exist.
- `Markers` can also serve `slice()` (--at/--from/--to) by locating
  checkpoints and filtering segments by range.
- For `journal_cmd` display, iterate all segments interleaved with their
  boundary CKP/RST records from `Markers`. Use `Markers::is_segment_alive()`
  for dimming. Since segments own the ADD/MOD/DEL/RDR/REP records and `Markers`
  owns the CKP/RST records, the full journal order is reconstructed by
  interleaving: emit marker, emit segment records, emit next marker, etc.
  Each `Marker` has a `pos` field that preserves original ordering.
- Restore workflow in detail: find checkpoint via `Markers` (searches
  all K markers), determine the prefix of segments up to that checkpoint,
  filter prefix to live (via `Markers`) producing `LiveSegments`, then
  resolve into a `Changeset`. This differs from other callers because
  the prefix truncation happens before the live filter.

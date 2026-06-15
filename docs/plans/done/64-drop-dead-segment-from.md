# 64 — Remove dead `Segment.from`; derive `latest_gen`

## Status: DONE — implemented and verified (`make test-vm`: 286 unit + 589 e2e pass)

Follow-up cleanup enabled by plan 58 (gen ≡ marker position). With gen no
longer threaded through the journal fold, two pieces of `Journal::new`
bookkeeping turn out to be dead or derivable.

## Findings

- **`Segment.from` has no production reader.** Every read is in
  `#[cfg(test)]` (core.rs test asserts + four test `build` helpers);
  `DirTree::build` and all CLI consumers use only `seg.records`. The field
  is carried through the fold purely to satisfy test assertions.
- **`latest_gen` is derivable.** Since gen ≡ position, the latest marker's
  gen is always `markers.len() - 1`. It is currently a `let mut` written in
  every marker arm; it can be computed once after the markers are built.
  Only consumer: `cmd/mount.rs` (re-seeds RESTORE).

Together these remove `current_from`, the per-arm `gen_id`/`latest_gen`
writes, and the `target_gen` binding — so the `Snapshot` and `Travel` match
arms become identical and merge into one.

## Changes

- **`user/journal/types.rs`** — drop the `from: u64` field (and its doc) from
  `Segment`; keep `records`.
- **`user/journal/core.rs` (`Journal::new`)** — remove `current_from` and the
  `let mut latest_gen`; collapse both marker arms to a single
  `Record::Marker(marker)` arm (push segment, push marker, reset
  `dirty`, set `cow_ino_floor`); after the loop and `MarkerIndex::new`,
  compute `let latest_gen = markers.len() as u64 - 1` (phantom at 0 ⇒ 0 when
  no real markers).
- **Tests** — remove the `j.segments[N].from` / `live[0].from` assertions in
  `core.rs`; drop `from: 0,` from the four test `build` helpers
  (`cmd/travel.rs`, `journal/plan.rs`, `journal/tree.rs` ×2). `latest_gen`
  assertions are unchanged (same values).

No wire-format, kernel, or docs/staging.md change — `Segment` is an
in-memory CLI type only.

## Validation

`make test-vm`.

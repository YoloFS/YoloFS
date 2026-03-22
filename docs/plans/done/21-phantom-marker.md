# 21 — Phantom Marker (align segment_index == gen_id)

## Problem

Segments and markers are off-by-one: segment 0 (pre-checkpoint records) has
no corresponding marker, so `segments.len() == markers.len() + 1` and
`marker[i].gen_id == i + 1`.  Every index conversion requires `±1` adjustments
that are easy to get wrong.

## Approach

Insert a **phantom marker** `Marker::Checkpoint { gen_id: 0, name: "(initial)" }` at
index 0 in `Markers` during `Journal::new()`.  After this change:

- `markers.len() == segments.len()`
- `marker[i].gen_id == i`  (direct indexing, no offset)
- `segment[i]` corresponds to `marker[i]`

The phantom is purely a CLI-side construct — no kernel changes.

## Changes

### 1. journal.rs — `Journal::new()`
Insert phantom at position 0 of the markers vec before constructing `Markers`.

### 2. journal.rs — `into_live_segments_at()`
- Use `gen_id as usize` directly (was clamped to `markers.len()`). Callers
  must pass valid gen_ids.

### 3. markers.rs — `find_checkpoint_by_gen_id()`
- Remove the `gen_id.checked_sub(1)` offset; use `gen_id` directly as the index.
- Return error for gen_id == 0 (phantom is not a real checkpoint).

### 4. markers.rs — `segment_range()` "at" clause
- `m_idx = gen_id as usize` (was `gen_id - 1`).
- `prev_k` search range: `(1..m_idx)` (was `0..m_idx`; skip phantom at 0).
- `start = prev_k.unwrap_or(0)` (was `prev_k.map(|k| k + 1).unwrap_or(0)`).
- `end = m_idx` (was `m_idx + 1`).

### 5. markers.rs — `alive_segments_range()`
- Kill range: `k_idx..m` (was `(k_idx + 1)..=m`).
- Guard: `m >= alive_end` (was `m + 1 >= alive_end`).
- `alive_end = k_idx` (was `k_idx + 1`).

### 6. markers.rs — `checkpoint_at()`
- Return `None` for the phantom (gen_id == 0).

### 7. markers.rs — update gen_id invariant comment
- `marker[i].gen_id = i` (was `i + 1`).

### 8. audit.rs
- Empty check: `markers.len() <= 1` (was `markers.is_empty()`).
- Print marker after segment: `markers.get(seg_idx + 1)` (was `markers.get(seg_idx)`).

### 9. timeline.rs
- Empty check: `markers.len() <= 1` (was `markers.is_empty()`).
- Skip phantom: `if m_idx == 0 { continue; }`.
- Reachability: `is_alive(m_idx - 1)` (was `is_alive(m_idx)`).

### 10. diff.rs — `checkpoint_at()` semantics
- `checkpoint_at(i)` means "the checkpoint that starts segment i" — use
  directly (no `i + 1`).
- Drop the "unsaved changes" indicator; no longer needed.

### 11. Tests
- Update any tests with hard-coded marker index expectations.
- Verify segment_range tests still produce correct ranges.

### 12. Documentation
- Update `docs/staging.md` gen_id invariant description.

# 32 — Restore to any marker

## Problem

`agfs restore` only accepts checkpoint markers. You cannot jump to a
restore marker by gen_id. Jumping to a restore marker is meaningful
because it produces a narrower set of unreachable markers than jumping
to the restore's target checkpoint — it preserves more history.

## Approach

Generalize "checkpoint" to "marker" in the lookup and liveness APIs so
that any gen_id (checkpoint or restore) is a valid jump target.

Jumping to a restore marker means jumping to that position in the
timeline. The liveness algorithm naturally computes the correct state —
no special-casing needed. The unreachable region starts at the target
gen_id, which is narrower than jumping to the original checkpoint.

## Changes

### 1. `cli/journal/markers.rs` — Rename and generalize lookup

- **`find_checkpoint` → `find_marker`**: accept both Checkpoint and
  Restore markers when looked up by numeric gen_id. Name-based lookup
  still only matches checkpoints (restores have no names). Return type
  changes from `Result<(u64, &str)>` to `Result<u64>` (just the gen_id).

- **`find_checkpoint_by_gen_id` → `find_marker_by_gen_id`**: match
  `Marker::Checkpoint { .. } | Marker::Restore { .. }`.

- **`find_checkpoint_by_name`**: keep as-is (private helper), but callers
  go through `find_marker` which tries numeric first, then name.

- **`checkpoint_at` → `marker_at`**: return `Option<&Marker>` instead of
  `Option<(u64, &str)>`. Callers (diff.rs) pattern-match to extract what
  they need.

- **`segment_range`**: update calls from `find_checkpoint` to
  `find_marker`.

### 2. `cli/journal/markers.rs` — Liveness: index all markers

In `alive_segments_range`, add restore markers to `gen_to_idx` alongside
checkpoints. This lets a restore that targets another restore marker
resolve correctly for dead-zone computation.

```rust
// Before:
if let Marker::Checkpoint { gen_id, .. } = &self.0[i] { ... }

// After: index both checkpoint and restore markers
let gen = match &self.0[i] {
    Marker::Checkpoint { gen_id, .. } | Marker::Restore { gen_id, .. } => *gen_id,
};
gen_to_idx.insert(gen, i);
```

### 3. `cli/journal/core.rs` — `into_tree_at` includes target marker

Change `into_live_segments_at` so the marker range includes the marker at
`gen_id` itself (extend to `gen_id + 1`). This ensures a restore marker's
dead zone is applied when building the tree at that marker.

Also change `alive_segments_range` to initialize `alive_end` from
`range.end` instead of `num_segments`, so restore markers at the boundary
are processed.

```rust
fn into_live_segments_at(self, gen_id: u64) -> impl Iterator<Item = Segment> {
    let num_prefix = (gen_id as usize).min(self.segments.len());
    let marker_end = (gen_id as usize + 1).min(self.markers.len());
    let alive = self.markers.alive_segments_range(0..marker_end, num_prefix);
    // ...
}
```

### 4. `cli/cmd/restore.rs` — Use `find_marker`

- Call `find_marker` instead of `find_checkpoint`.
- For the success message: pattern-match the marker to show either
  `Restored to checkpoint "name"` or `Restored to marker [gen_id]`.
- The ioctl target_gen is always the marker's own gen_id.

### 5. `cli/cmd/diff.rs` — Update `checkpoint_at` → `marker_at`

Adapt the label computation. Pattern-match the returned `&Marker` to
extract `(gen_id, name)` for checkpoints or `(gen_id, "restored to [N]")`
for restore markers.

### 6. `docs/cli.md` — Update documentation

- `agfs restore <name|gen>` accepts any marker (checkpoint or restore),
  not just checkpoints.
- `--at`, `--from`, `--to` accept any marker gen_id.

### 7. Tests

- **Unit test** (`markers.rs`): `find_marker` with a restore marker gen_id.
- **Unit test** (`markers.rs`): liveness with restore-to-restore chain.
- **Unit test** (`core.rs`): `into_tree_at` with a restore marker gen_id.
- **E2E test** (`tests/cli/`): `agfs restore <restore_gen_id>` succeeds
  and produces the correct state.

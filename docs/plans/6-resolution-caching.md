# Plan: Cache Intermediate Results During Journal Resolution

## Problem

Every CLI invocation (`status`, `diff`, `commit`, `restore`, `abort`)
re-reads the entire journal from disk and replays all reachable records
through the resolver from scratch. The pipeline is:

```
fs::read(".agfs/journal")          O(file size)
  → parse all records              O(N records)
  → compute reachable ranges       O(N) + O(R restores)
  → resolve (replay state machine) O(reachable records)
  → emit Changes                   O(paths)
```

For long sessions with many checkpoints this becomes noticeable.
Checkpoints are natural cache boundaries — no mutations can occur during
a checkpoint (all fds are closed), so cached state at a checkpoint is
immutable and never needs invalidation.

## What to Cache

Three layers, each building on the previous:

### Layer 1: S/K Position Index

**Key:** journal byte length (u64)
**Value:** `{ s_list: Vec<(record_index, gen, target_gen)>, k_map: Vec<(gen, record_index)> }`
**Benefit:** Skip the O(N) scan for S/K positions in `compute_reachable_ranges()`. When the journal grows (append-only), parse only the new bytes and merge into the cached index.

### Layer 2: Resolver State at Checkpoint Boundaries

**Key:** checkpoint gen_id (u64)
**Value:** serialized `BTreeMap<String, Dirent>` — the resolver's internal state after processing all reachable records up to and including that checkpoint. After the Action/Dirent merge (now `Dirent`), this is the same type that `into_changes()` drains, so a cached state can be resumed directly.
**Benefit:** Instead of replaying the entire reachable prefix, load the cached resolver state at the latest reachable checkpoint before the region of interest, then replay only the trailing records.

| Operation | Without cache | With cache |
|-----------|--------------|------------|
| `status` / `diff` (no flags) | O(all reachable) | O(trailing unsaved) |
| `status --at C` | O(records up to C) | O(segment C only) |
| `commit` | O(all reachable) | O(trailing unsaved) |
| `restore <C>` | O(prefix up to C) | O(1) load |

### Layer 3: Resolved Segment Changes

**Key:** `(from_gen, to_gen)` — the two bounding checkpoints
**Value:** serialized `Vec<Dirent>` — the final collapsed changes for that segment.
**Benefit:** `status`/`diff` can load pre-resolved segments directly without running the resolver at all for completed segments. Only the trailing (unsaved) segment needs resolution.

## Cache Properties

- **Immutable by design.** Checkpoint caches are computed once and never
  change. No open fds during checkpoint means no mutations can slip in.
- **No invalidation needed for restores.** S records make some segments
  unreachable, but those caches simply go unused — they don't need to be
  deleted. If a later restore "undoes" the first, the caches become
  reachable again automatically.
- **Commit/abort clears all caches.** These are session boundaries that
  truncate the journal and reset state.
- **Eager build.** Compute and persist caches right after
  `agfs checkpoint` returns, while the resolver state is still in
  memory.

## Storage Format

```
.agfs/cache/
  positions.bin          # Layer 1: S/K index
  resolver/<gen>.bin     # Layer 2: resolver state per checkpoint
  segments/<from>-<to>.bin  # Layer 3: resolved changes per segment
```

Use `bincode` or a simple custom binary format. Caches are ephemeral
and version-locked to the CLI binary — include a format version byte
and discard on mismatch.

## Integration Points

### `agfs checkpoint` (eager cache build)

After the kernel appends the K record and the CLI confirms success:

1. Re-read the journal (or use the records already in memory).
2. Compute and write `positions.bin` (Layer 1).
3. Serialize the resolver state and write `resolver/<gen>.bin` (Layer 2).
4. Resolve the just-completed segment and write
   `segments/<prev_gen>-<gen>.bin` (Layer 3).

### `agfs status` / `agfs diff`

1. Load `positions.bin`. Parse only new journal bytes (after cached
   length). Merge new S/K entries.
2. Compute reachable ranges from the (cached + new) S/K index.
3. For each segment in the requested range:
   - If `segments/<from>-<to>.bin` exists and the segment is complete
     (has a `to` checkpoint), load it.
   - Otherwise, load `resolver/<from>.bin` and replay trailing records.
4. Display changes.

### `agfs commit`

1. Load `resolver/<latest_reachable_checkpoint>.bin`.
2. Replay only the unsaved trailing records.
3. Resolve → apply changes → reset staging (clears cache dir).

### `agfs restore`

1. Load `resolver/<target>.bin` directly — it contains the exact state
   at the target checkpoint.
2. Convert to changes → entries → ioctl. No replay needed.

### `agfs abort`

No caching benefit — abort just wipes everything including the cache.

## Serialization of Resolver State

After the Action/Dirent merge (now `Dirent`) (plan 3), the `Resolver` holds a
`BTreeMap<String, Dirent>` directly — `Dirent` is both the internal
accumulator and the output type:

```rust
pub enum Dirent {
    Added { ino: u64, dtype: DType },
    Modified { ino: u64, dtype: DType },
    Deleted,
    Renamed { from: String, dtype: DType },
    Replaced { from: String, dtype: DType },
}
```

This simplifies caching significantly:

- **One type to serialize.** The resolver state and the segment cache
  use the same `Dirent` enum — no conversion layer between internal
  and external representations.
- **`into_changes()` is just `drain().collect()`.** The cached
  `BTreeMap<String, Dirent>` can be resumed directly without
  reconstructing an intermediate type.
- **Segment caches reuse the same format.** A cached segment is
  `Vec<(String, Dirent)>` — identical to what `into_changes()` returns.

All fields are simple scalars and strings — straightforward to
serialize. `DType` is a 3-variant enum (File/Dir/Link).

Consider deriving `serde::Serialize`/`Deserialize` on `Dirent` and
`DType` behind a `cache` feature flag, or writing a manual compact
binary format.

## Todos

| ID | Task | Files | Depends on |
|----|------|-------|------------|
| cache-infra | Create `.agfs/cache/` dir, version header, read/write helpers | `cli/cache.rs` (new) | — |
| cache-clear | Clear cache on commit and abort | `cli/abort.rs`, `cli/commit.rs` | cache-infra |
| cache-positions | S/K position index with incremental update | `cli/cache.rs`, `cli/journal/timeline.rs` | cache-infra |
| cache-resolver | Serialize/deserialize `BTreeMap<String, Dirent>`; write at checkpoint time | `cli/journal/resolve.rs`, `cli/checkpoint.rs` | cache-infra |
| cache-segments | Serialize/deserialize `Vec<(String, Dirent)>` per segment | `cli/journal/resolve.rs`, `cli/checkpoint.rs` | cache-resolver |
| cache-read-commit | Load cached resolver state in commit path | `cli/commit.rs` | cache-resolver |
| cache-read-status | Load cached segments in status/diff path | `cli/diff.rs` | cache-segments |
| cache-read-restore | Load cached resolver state in restore path | `cli/restore.rs` | cache-resolver |
| cache-eager | Eagerly build all caches in `agfs checkpoint` | `cli/checkpoint.rs` | cache-positions, cache-resolver, cache-segments |
| cache-tests | Unit tests: cache hit/miss, version mismatch, corrupt cache, post-restore cache reuse | `cli/cache.rs` | cache-eager |
| cache-bench | Benchmark before/after on large journals | `bench/` | cache-tests |

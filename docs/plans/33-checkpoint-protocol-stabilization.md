# Checkpoint Protocol Stabilization for Bench Backends

## Goals

- Keep checkpoint orchestration in backend code instead of workload-specific backend logic.
- Make `checkpoint-scalability` robust when a backend reaches checkpoint depth limits.
- Preserve existing behavior for non-checkpoint workloads and metadata checkpoint-source workloads.

## Plan

1. Extend the backend checkpoint controller contract to support two outcomes:
   - checkpoint succeeded with measured latency,
   - checkpoint cannot continue and workload should stop collecting more points.
2. Extend the checkpoint request/response subprocess protocol so workloads can
   receive a backend-provided stop signal without treating it as a hard failure.
3. Update `checkpoint-scalability` workload loop to stop early on that signal
   and emit a partial series.
4. Update overlayfs checkpoint controller to convert lowerdir/remount depth
   failures into a graceful stop signal for this workload.
5. Fix branchfs checkpoint parent tracking to follow the currently active
   checkpoint branch across requests.
6. Validate with bounded-step benchmark runs (timeouts + small step count)
   across agfs/overlayfs/branchfs.

## Validation

- `cargo build -p agfs-bench`
- `AGFS_BENCH_CHECKPOINT_STEPS=20 timeout ... agfs-bench --micro --workload checkpoint-scalability --backend <backend> --runs 1`

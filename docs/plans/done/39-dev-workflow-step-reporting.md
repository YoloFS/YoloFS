# 39 — Dev-Workflow Step Reporting

## Problem

`dev-workflow` executes a detailed search/read/edit/build/commit session, but
the benchmark pipeline still records it as one opaque macro workload iteration.
The runtime already measures total init/staging/commit, yet the report does not
show how that total breaks down by step category.

## Approach

- Teach `dev-workflow` to emit an ordered per-step timing series at the end of
  each run.
- Thread that series through the existing subprocess/result pipeline.
- Aggregate the series across benchmark iterations when the step sequence is
  stable.
- Render a dedicated `dev-workflow` report that collapses detailed steps into
  the documented categories (`worktree`, `config`, `initial-build`, `search`,
  `read`, `edit`, `incremental-build`, `git-*`, `checkpoint`) and plots them as
  stacked backend bars.

## Changes

### 1. Docs first

- Update `docs/benchmark.md` to state that `dev-workflow` stores ordered step
  timings in results JSON and renders a stacked breakdown plot from the
  aggregated step series.

### 2. Result schema

- Add a generic macro step timing type under `bench/src/workload.rs`, for
  example:
  - `MacroStepTiming { step: String, ms: u64 }`
  - `MacroStepSeries { steps: Vec<MacroStepTiming> }`
- Add optional fields for that series to:
  - subprocess results
  - iteration results
  - aggregated backend results

### 3. Subprocess protocol

- Extend the stdout result parser in `bench/src/backend.rs` so a workload may
  emit:
  - `OpResult`
  - `CheckpointLatencySeries`
  - `MacroStepSeries`
- Preserve existing behavior for non-`dev-workflow` workloads.

### 4. Workload instrumentation

- Instrument `bench/src/workloads/dev_workflow.rs` to record step timings for:
  - `worktree`
  - `config`
  - `initial-build`
  - per-command `search`
  - per-command `read`
  - per-command `edit`
  - per-checkpoint `checkpoint`
  - per-commit `incremental-build`
  - `git-status`
  - `git-diff`
  - `git-add`
  - `git-commit`
- Emit a final `MacroStepSeries` payload through the existing
  `AGFS_BENCH_RESULTS` channel.

### 5. Aggregation and reporting

- Aggregate macro step series across runs when the ordered step names match.
- Add a `report.rs` special-case renderer for `dev-workflow` that:
  - sums detailed steps into the documented categories
  - renders a stacked bar chart (one bar per backend)
  - keeps detailed per-step hover text available

### 6. Validation

- Add or extend tests to cover:
  - `MacroStepSeries` parsing
  - aggregation of identical step sequences
  - `dev-workflow` fixture parsing remaining intact
- Verify with:
  - `cargo test -p agfs-bench --no-run`
  - targeted unit tests for the new aggregation/parser code
  - one `native` and one `agfs-realistic` `dev-workflow` run producing the new
    report output

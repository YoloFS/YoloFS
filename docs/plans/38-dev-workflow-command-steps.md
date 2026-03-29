# 38 — Dev-Workflow Command Steps

## Problem

`dev-workflow` currently executes checked-in `search`/`read` shell scripts and
Python edit fixtures. That keeps the benchmark reproducible, but it hides the
actual operation stream behind opaque helper code:

- edit timing is still defined by fixture-side Python logic instead of explicit
  tool invocations
- the workload cannot naturally checkpoint after each individual edit command
- the benchmark doc says the workflow is a `grep`/`sed`/build/git loop, but the
  implementation still depends on Python-side text transforms

The benchmark should reflect what it claims to measure: a command-driven
developer workflow whose search, read, and edit stages are all explicit shell
commands.

## Approach

- Replace per-commit `search_script`, `read_script`, and `edit_script` fixture
  entries with checked-in command lists.
- Execute each command directly from Rust via `bash -lc`, one command at a
  time.
- Keep per-edit checkpoints by checkpointing after each edit command.
- Remove Python edit helpers from the runtime path.

## Changes

### 1. Docs first

- Update `docs/benchmark.md` to describe `dev-workflow` as checked-in command
  lists, not scripts plus declarative/Python edit logic.
- Clarify that the edit stage is replayed as explicit text-tool commands
  (`sed`, `perl`, etc.) and checkpointed after each command.

### 2. Fixture format

- Update `bench/fixtures/dev-workflow/overlayfs-ovl-file.json` so each commit
  contains:
  - `search`: array of shell command strings
  - `read`: array of shell command strings
  - `edit`: array of shell command strings
- Preserve the existing pinned base commit, commit ids, touched-file list, and
  commit message.
- Express each edit as one shell snippet that mutates exactly one source region
  and can be checkpointed independently, using `sed -i` for simple local edits
  and `patch` for larger block rewrites.

### 3. Runner

- Refactor `bench/src/workloads/dev_workflow.rs` to load command arrays instead
  of script paths.
- Add a helper that runs one shell command in the worktree (`bash -lc`).
- Execute `search` and `read` commands in order.
- Execute edit commands one by one, checkpointing after each successful command.
- Remove the Python-specific `edit_op_count` / `run_script_args` path.

### 4. Tests / validation

- Add a small bench-side test that fixture parsing succeeds and each commit has
  non-empty `search`, `read`, and `edit` command lists.
- Validate with `cargo test -p agfs-bench --no-run`.
- Re-run the pinned Linux replay from `c2c54b5f34f6^` and confirm that the
  command lists still reproduce each upstream commit exactly.

## Files touched

- `docs/benchmark.md`
- `docs/plans/38-dev-workflow-command-steps.md`
- `bench/src/workloads/dev_workflow.rs`
- `bench/fixtures/dev-workflow/overlayfs-ovl-file.json`
- `bench/fixtures/dev-workflow/*` (remove obsolete script helpers if no longer
  referenced)

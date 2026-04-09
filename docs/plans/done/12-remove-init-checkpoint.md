# Remove Init Checkpoint

## Problem

On mount, the kernel writes an implicit `(initial)` checkpoint (gen=1) to the
journal. This exists purely as a user-facing convenience so users can
`yolo restore 1` to get back to mount-time state. However, `commit` and `abort`
already reset to clean state via `target_gen=0`, and the journal system supports
a 0th segment (`from: None`) for records before any checkpoint. The init
checkpoint adds complexity without functional value.

## Approach

Remove the init checkpoint entirely — kernel, docs, tests.

## Changes

### Kernel
- `kmod/super.c`: Remove the `yolo_journal_checkpoint(sbi, 1, "(initial)")` call
  and its comment.

### Documentation
- `docs/staging.md`: Remove all references to the init checkpoint, the
  `K\01\0(initial)\n` record, and the "(initial)" row in example tables.
- `docs/cli.md`: Remove `checkpoint [1] (initial)` from timeline example,
  renumber remaining entries.
- `docs/plans/done/0-append-only-journal-with-restore-records.md`: Update
  references (done plan, lighter touch).

### Tests — Remove
- `tests/cli/test_restore.rs`: Remove `restore_to_initial`,
  `restore_to_initial_by_name`, `restore_to_initial_after_restore`.
- `tests/cli/test_diff.rs`: Remove `diff_after_restore_to_initial`.
- `tests/cli/test_status.rs`: Remove `status_after_restore_to_initial`.

### Tests — Update
- `tests/internals/test_checkpoint.rs`: Remove the filter that skips
  `"(initial)"` — it will no longer appear.
- `cli/journal/liveness.rs`: Update `reachable_restore_to_initial` test —
  either remove or rewrite to use a regular checkpoint.

### CLI source
- No production CLI code changes needed (no special-casing of init checkpoint).

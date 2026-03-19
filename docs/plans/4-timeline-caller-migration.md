# Plan: Migrate CLI Commands to Use `Timeline` Struct

## Problem

The `Timeline` struct in `cli/journal/timeline.rs` provides a structured
view of journal records with segment boundaries and reachability, but no
production CLI commands use it yet. All commands use the flat functions
(`reachable()`, `reachable_indices()`, `slice_records()`,
`find_checkpoint_index()`) directly.

This means:
- Multiple commands independently compute reachability.
- The `Timeline` struct (and its segment-based iteration) is only
  exercised by its own unit tests.
- There is code duplication between `Timeline` methods and the flat
  functions that both call `compute_reachable_ranges()`.

## Approach

Migrate each CLI command to construct a `Timeline` once and use its
methods instead of calling flat functions. After migration, the flat
`reachable()` and `reachable_indices()` functions can be removed or made
`pub(crate)` implementation details.

## Todos

| ID | Task | Files |
|----|------|-------|
| tl-journal-cmd | Use `Timeline` in `agfs journal` instead of `reachable_indices()` | `cli/journal_cmd.rs` |
| tl-timeline-cmd | Use `Timeline` in `agfs timeline` instead of `reachable_indices()` | `cli/timeline_cmd.rs` |
| tl-diff | Use `Timeline::slice()` + resolve in `agfs diff`/`agfs status` | `cli/diff.rs` |
| tl-commit | Use `Timeline` in `agfs commit` | `cli/commit.rs` |
| tl-restore | Use `Timeline::find_checkpoint()` + reachable prefix in `agfs restore` | `cli/restore.rs` |
| tl-abort | Use `Timeline` in `agfs abort` | `cli/abort.rs` |
| tl-mount | Use `Timeline` in mount prompt | `cli/mount.rs` |
| tl-cleanup | Remove or reduce visibility of flat `reachable()`, `reachable_indices()`, `find_checkpoint_index()`, `slice_records()` | `cli/journal/timeline.rs` |

## Notes

- `restore.rs` needs `find_checkpoint` across ALL records (including
  unreachable), which `Timeline::find_checkpoint()` already supports.
- `diff.rs` currently calls `reachable()` → `slice_records()` →
  `resolve_segments()`. This becomes `Timeline::slice()` →
  `resolve_segments()`.
- `commit.rs` and `abort.rs` only need `reachable()` + `resolve()`,
  which becomes `timeline.reachable_records()` + `resolve()`.

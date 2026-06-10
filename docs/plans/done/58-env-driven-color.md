# 57 — Env/tty-driven color (drop the forced override)

## Problem

`main.rs` calls `colored::control::set_override(true)` unconditionally, so
every invocation emits ANSI codes — even with stdout piped to a file and
even with `NO_COLOR=1` set (the e2e harness sets it; it is silently dead).
Tests that parse line structure need an ANSI stripper
(`tests/helpers.rs::strip_ansi`), and `yolo review > changes.txt` captures
escape codes.

The override exists for one consumer: `example.sh` mirrors the walkthrough
through `tee`, so stdout/stderr are pipes and tty detection would disable
color for the terminal copy.

## Design

Delete the override and let `colored` 2.2's built-in policy decide, in its
documented priority order: `CLICOLOR_FORCE` (force on) > `NO_COLOR` (force
off) > `CLICOLOR` combined with a tty check. `example.sh` exports
`CLICOLOR_FORCE=1` to keep color through its `tee` pipeline.

Consequences:

- Piped/captured output is plain by default — standard CLI behavior.
- The harness's `NO_COLOR=1` becomes redundant (the tty check already
  yields plain output through the test pipes) — remove it from
  `tests/helpers.rs`.
- `tests/helpers.rs::strip_ansi` becomes dead — remove it and the `.map`
  in `status_lists_changes_in_sorted_order`.
- `colored`'s tty check looks at stdout only, so `yolo review > file` also
  drops color from stderr status lines. Acceptable: matching the data
  stream is the conventional behavior, and `CLICOLOR_FORCE=1` restores it.
- The `report.rs` unit test sets its own explicit overrides — unaffected.

## Steps

1. **Docs first.** `docs/cli.md`, "Output and status reporting": state the
   color policy (tty-detected; `NO_COLOR` disables, `CLICOLOR_FORCE=1`
   forces, e.g. for capturing colored output through a pipe).
2. **Implement.** Remove `set_override(true)` from `main.rs`; add
   `export CLICOLOR_FORCE=1` to `example.sh` (top, near the tee exec).
3. **Tests.** New e2e test in `tests/cli/test_status.rs`: review output
   through the (piped) harness contains no `\x1b` by default; with
   `CLICOLOR_FORCE=1` in the environment it does. Remove `strip_ansi` and
   the redundant `NO_COLOR=1` env from `tests/helpers.rs` and simplify
   `status_lists_changes_in_sorted_order`.
4. **Regenerate `example.out`** (`./example.sh`) — expected byte-identical
   (the file copy strips ANSI; only the terminal copy keeps color).
5. `make test-vm`, `make lint`.
6. **Code review** per AGENTS.md, then move this plan to `docs/plans/done/`.

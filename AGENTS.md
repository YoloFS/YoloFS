# AGENTS.md — Coding Guidelines for AgFS

## Principles

- Keep code simple and easy to understand.
- No backwards compatibility needed — remove deprecated code.
- Do not repeat the same code — extract shared logic.
- Think through problems and implement the best solution; avoid fallbacks.
- Keep functions short. Extract helpers when a function does multiple distinct things.

## Workflow

- Always update documentation (`docs/`) before implementation.
- Always run tests (unit and e2e) to verify changes.
- To fix a bug, first write a failing test, then fix it.
- Do not modify existing tests when fixing a bug. If you are unsure, ask.
- Do not use git (commit, push, rebase, etc.) unless explicitly asked.

## Project Structure

- **kmod/** — Linux kernel module (C). Build with `make kmod`.
- **cli/** — Userspace CLI (Rust). Build with `make cli`.
  Unit tests live inline via `#[cfg(test)]` in each module.
- **tests/** — E2E / integration tests.
  - `tests/fs/` — black-box filesystem behavior through the mount via `std::fs`.
  - `tests/cli/` — black-box; run `agfs <subcommand>` and assert on stdout/stderr/exit-code.
  - `tests/perm/` — black-box permission rule enforcement.
  - `tests/internals/` — white-box; inspect `.agfs/inodes/` and `.agfs/journal` directly.
- **docs/** — Design documents (architecture.md, cli.md, internals.md, permissions.md, staging.md, benchmark.md). Keep in sync with code.

## Build & Test

```bash
make vm-build     # build cli + kmod
make vm-test      # unit + e2e tests (auto-starts VM if needed)
make vm-test-unit # unit tests only
make vm-test-e2e  # e2e tests only
```

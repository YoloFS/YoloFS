# AGENTS.md — Coding Guidelines for AgFS

## Principles

- Keep code simple and easy to understand.
- No backwards compatibility needed — remove deprecated code. Freely change interfaces and update all callers.
- Do not repeat the same code — extract shared logic.
- Think through problems and implement the best solution; avoid fallbacks.
- Keep functions short. Extract helpers when a function does multiple distinct things.
- Do not change or remove existing comments unless they are outdated.
- Do not be afraid of large-scale refactoring. If the design is better, do it.

## Workflow

- Always update documentation (`docs/`) before implementation.
- Always run tests in vm to verify changes.
- To fix a bug, first write a failing test, then fix it.
- Do not modify existing tests when fixing a bug. If you are unsure, ask.
- When adding new features or making changes, add tests if applicable: unit tests (inline `#[cfg(test)]`), white-box tests (`tests/internals/`), and black-box tests (`tests/fs/`, `tests/cli/`, `tests/perm/`).
- Do not use git (commit, push, rebase, etc.) unless explicitly asked.
- For refactoring, save a plan under `docs/plans/` (numbered: `0-name.md`, `1-name.md`, ...) before implementing. When the plan is fully implemented, move it to `docs/plans/done/`.
- Before finalizing changes, run a code review (see **Code Review** section below).

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

## Code Review

Before finalizing any change set, run a full review of the current changes. Launch all review checks **in parallel as separate sub-agents**. Each sub-agent examines `git diff` for one category:

1. **Bugs & correctness** — logic errors, off-by-one, unhandled errors, null/unwrap panics, race conditions, use-after-free, unsafe code, unchecked inputs.
2. **Code quality** — unnecessary allocations/clones, redundant operations, overly complex logic, code that could be simplified or deduplicated, more idiomatic Rust/C patterns.
3. **Doc consistency** — do `docs/` files accurately describe the new behavior? Do they contradict each other or the code?
4. **Missing tests** — new code paths, features, or edge cases without test coverage; existing tests that need updating.

Each sub-agent reports findings with file paths and line references. After all agents finish, triage the results and address issues.

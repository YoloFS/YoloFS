# AGENTS.md — Coding Guidelines for YoloFS Filesystem

See [`../AGENTS.md`](../AGENTS.md) for cross-repo principles. The rules
below are filesystem-specific and stack on top.

## Principles

- Freely refactor and change any interface — rename, restructure, move, split, or merge. No backwards compatibility needed. This includes the kernel-userspace contract (ioctls, shared structs, protocol), kernel-internal interfaces, and userspace-internal interfaces.

## Workflow

**Required gates — do not skip, even for small changes:**

1. **Docs before implementation.** Update the relevant `docs/` files first, then write code to match.
2. **Plan before refactoring.** Save a numbered plan under `docs/plans/` (`0-name.md`, `1-name.md`, ...) *before* implementing; move it to `docs/plans/done/` once fully implemented.
3. **Failing test before bug fix.** Reproduce the bug with a failing test first, then fix it. Do not modify existing tests while fixing a bug — if unsure, ask.
4. **Code review before finalizing.** Run the full review in the **Code Review** section below before considering any change set done.

Supporting rules:

- Always verify changes with `make test`.
- When adding new features or making changes, add tests if applicable: unit tests (inline `#[cfg(test)]`), white-box tests (`tests/internals/`), and black-box tests (`tests/fs/`, `tests/cli/`, `tests/perm/`).
- Do not use git (commit, push, rebase, etc.) unless explicitly asked.
- Do not read or maintain old plans in `docs/plans/done/`. They are kept for historical reference only.

## Project Structure

- **kmod/** — Linux kernel module (C). Build with `make kmod`.
- **user/** — Userspace CLI (Rust). Build with `make user`.
  Unit tests live inline via `#[cfg(test)]` in each module.
- **tests/** — E2E / integration tests.
  - `tests/fs/` — black-box filesystem behavior through the mount via `std::fs`.
  - `tests/cli/` — black-box; run `yolo <subcommand>` and assert on stdout/stderr/exit-code.
  - `tests/perm/` — black-box permission rule enforcement.
  - `tests/internals/` — white-box; inspect `.yolofs/inodes/` and `.yolofs/journal` directly.
- **docs/** — Design documents (architecture.md, cli.md, internals.md, permissions.md, staging.md). Keep in sync with code.
- **user/templates/** — The project skeleton `yolo init` scaffolds, embedded into the binary at compile time (`include_str!`): the default `yolofs.toml` (also `config::DEFAULT_CONFIG`) plus drop-in hook templates (`.claude/`, `.gemini/`, `.github/`) that wrap a coding agent's shell commands so they run through YoloFS.
- **example/** — Generated, git-ignored. `example.sh` runs `yolo init example` to scaffold it from `user/templates/`, walks through the CLI there, then removes it. `example.out` is the captured walkthrough output.

## Build & Test

```bash
make user         # build userspace binary (host)
make test-unit    # unit tests
make test-e2e     # e2e tests
make test         # run all tests (unit + e2e)
```

## Code Review

Before finalizing any change set, run a full review of the current changes. Launch all review checks **in parallel as separate sub-agents**. If the user specifies a commit to review, use `git show <commit>` for the diff; otherwise, use `git diff` for unstaged changes. Each sub-agent examines the diff for one category:

1. **Bugs & correctness** — logic errors, off-by-one, unhandled errors, null/unwrap panics, race conditions, use-after-free, unsafe code, unchecked inputs.
2. **Code quality** — unnecessary allocations/clones, redundant operations, overly complex logic, code that could be simplified or deduplicated, more idiomatic Rust/C patterns.
3. **Doc consistency** — do `docs/` files accurately describe the new behavior? Do they contradict each other or the code?
4. **Missing tests** — new code paths, features, or edge cases without test coverage; existing tests that need updating.
5. **Plan adherence** — if a corresponding plan exists in `docs/plans/`, verify the changes follow the plan and flag anything specified in the plan that has not been implemented yet.

Each sub-agent reports findings with file paths and line references. After all agents finish, triage the results and address issues.


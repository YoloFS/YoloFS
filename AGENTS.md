# AGENTS.md — Coding Guidelines for YoloFS Filesystem

See [`../AGENTS.md`](../AGENTS.md) for cross-repo principles. The rules
below are filesystem-specific and stack on top.

## Principles

- Freely refactor and change any interface — rename, restructure, move, split, or merge. No backwards compatibility needed. This includes the kernel-userspace contract (ioctls, shared structs, protocol), kernel-internal interfaces, and userspace-internal interfaces.

## Workflow

- Always update documentation (`docs/`) before implementation.
- Always verify changes with `make test-vm` (unit tests on host, e2e tests in VM).
- To fix a bug, first write a failing test, then fix it.
- Do not modify existing tests when fixing a bug. If you are unsure, ask.
- When adding new features or making changes, add tests if applicable: unit tests (inline `#[cfg(test)]`), white-box tests (`tests/internals/`), and black-box tests (`tests/fs/`, `tests/cli/`, `tests/perm/`).
- Do not use git (commit, push, rebase, etc.) unless explicitly asked.
- For refactoring, save a plan under `docs/plans/` (numbered: `0-name.md`, `1-name.md`, ...) before implementing. When the plan is fully implemented, move it to `docs/plans/done/`.
- Do not read or maintain old plans in `docs/plans/done/`. They are kept for historical reference only.
- Before finalizing changes, run a code review (see **Code Review** section below).

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
make test-unit    # unit tests (host; pure userspace)
make test-e2e-vm  # build + install + run e2e tests in VM
make test-vm      # test-unit + test-e2e-vm
```

## Code Review

Before finalizing any change set, run a full review of the current changes. Launch all review checks **in parallel as separate sub-agents**. If the user specifies a commit to review, use `git show <commit>` for the diff; otherwise, use `git diff` for unstaged changes. Each sub-agent examines the diff for one category:

1. **Bugs & correctness** — logic errors, off-by-one, unhandled errors, null/unwrap panics, race conditions, use-after-free, unsafe code, unchecked inputs.
2. **Code quality** — unnecessary allocations/clones, redundant operations, overly complex logic, code that could be simplified or deduplicated, more idiomatic Rust/C patterns.
3. **Doc consistency** — do `docs/` files accurately describe the new behavior? Do they contradict each other or the code?
4. **Missing tests** — new code paths, features, or edge cases without test coverage; existing tests that need updating.
5. **Plan adherence** — if a corresponding plan exists in `docs/plans/`, verify the changes follow the plan and flag anything specified in the plan that has not been implemented yet.

Each sub-agent reports findings with file paths and line references. After all agents finish, triage the results and address issues.


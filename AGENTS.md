# AGENTS.md — Coding Guidelines for YoloFS Filesystem

See [`../AGENTS.md`](../AGENTS.md) for cross-repo principles and the code
review discipline. The rules below are filesystem-specific and stack on top.

## Scope

- This file applies to the YoloFS filesystem implementation: `kmod/`, `user/`,
  `tests/`, and `docs/`.

## Principles

- Freely refactor and change any interface — rename, restructure, move, split, or merge. No backwards compatibility needed. This includes the kernel-userspace contract (ioctls, shared structs, protocol), kernel-internal interfaces, and userspace-internal interfaces.

## Workflow

- Always update documentation (`docs/`) before implementation.
- Always run tests in vm to verify changes.
- To fix a bug, first write a failing test, then fix it.
- Do not modify existing tests when fixing a bug. If you are unsure, ask.
- When adding new features or making changes, add tests if applicable: unit tests (inline `#[cfg(test)]`), white-box tests (`tests/internals/`), and black-box tests (`tests/fs/`, `tests/cli/`, `tests/perm/`).
- Do not use git (commit, push, rebase, etc.) unless explicitly asked.
- For refactoring, save a plan under `docs/plans/` (numbered: `0-name.md`, `1-name.md`, ...) before implementing. When the plan is fully implemented, move it to `docs/plans/done/`.
- Do not read or maintain old plans in `docs/plans/done/`. They are kept for historical reference only.
- Before finalizing changes, run the code review described in [`../AGENTS.md`](../AGENTS.md).

## Project Structure

- **kmod/** — Linux kernel module (C). Build with `make kmod`.
- **user/** — Userspace CLI (Rust). Build with `make user`.
  Unit tests live inline via `#[cfg(test)]` in each module.
- **tests/** — E2E / integration tests.
  - `tests/fs/` — black-box filesystem behavior through the mount via `std::fs`.
  - `tests/cli/` — black-box; run `yolo <subcommand>` and assert on stdout/stderr/exit-code.
  - `tests/perm/` — black-box permission rule enforcement.
  - `tests/internals/` — white-box; inspect `.yolofs/inodes/` and `.yolofs/journal` directly.
- **docs/** — Design documents (architecture.md, cli.md, internals.md, permissions.md, staging.md, benchmark.md). Keep in sync with code.

## Build & Test

```bash
make vm-build     # build user + kmod
make vm-test      # unit + e2e tests (auto-starts VM if needed)
make vm-test-unit # unit tests only
make vm-test-e2e  # e2e tests only
```


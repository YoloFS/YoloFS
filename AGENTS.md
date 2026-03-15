# AGENTS.md — Coding Guidelines for AgFS

## Principles

- Keep code simple and easy to understand.
- No backwards compatibility needed — remove deprecated code.
- Do not repeat the same code — extract shared logic.
- Think through problems and implement the best solution; avoid fallbacks.

## Workflow

- Always update documentation (`docs/`) before implementation.
- Always run tests (unit and e2e) to verify changes.
- To fix a bug, first write a failing test, then fix it.
- Unless the test is wrong, do not modify existing tests when fixing a bug.

## Project Structure

- **kmod/** — Linux kernel module (C). Build with `make kmod`.
- **cli/** — Userspace CLI (Rust). Build with `make cli`.
  Unit tests live inline via `#[cfg(test)]` in each module.
- **tests/** — E2E / integration tests.
  - `tests/fs/` — black-box filesystem behavior through the mount via `std::fs`.
  - `tests/cli/` — black-box; run `agfs <subcommand>` and assert on stdout/stderr/exit-code.
  - `tests/perm/` — black-box permission rule enforcement.
  - `tests/internals/` — white-box; inspect `.agfs/staging/` and `.agfs/journal` directly.
- **docs/** — Design documents (architecture.md, cli.md, internals.md, permissions.md, staging.md, benchmark.md). Keep in sync with code.

## Build & Test

Tests run inside a QEMU VM managed by `vm.py` (repo auto-mounted via 9p).

```bash
./vm.py start                 # launch the VM
./vm.py ssh                   # interactive shell inside the VM
./vm.py ssh -- make test      # unit + e2e tests
./vm.py ssh -- make test-unit # unit tests only
./vm.py ssh -- make test-e2e  # e2e tests only
```

# AGENTS.md — Coding Guidelines for AgFS

## Principles

- Keep code simple and easy to understand.
- No backwards compatibility needed — remove deprecated code.
- Do not repeat the same code — extract shared logic.
- Think through problems and implement the best solution; avoid fallbacks.

## Workflow

- Always update documentation (DESIGN.md) before implementation.
- Always run `cargo test --lib` to verify changes.
- To fix a bug, first write a failing test, then fix it.
- Unless the test is wrong, do not modify existing tests when fixing a bug.

## Project Structure

- **kmod/** — Linux kernel module (C). Build with `make kmod`.
- **cli/** — Userspace CLI (Rust). Build with `make cli`.
- **tests/** — Integration tests.
- **DESIGN.md** — Authoritative design document. Keep in sync with code.

## Build & Test

```bash
make build          # build cli + kmod
make install        # install cli binary + kernel module
make test           # unit + integration tests
```

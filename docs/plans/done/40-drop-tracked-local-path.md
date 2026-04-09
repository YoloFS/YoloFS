## Why

The repo currently tracks `local` as a symlink, and multiple build / VM / CI
paths still go through that symlink. That creates brittle behavior outside the
author's machine, notably GitHub Actions cache restore failures when
`local/target` points through a missing symlink target.

## Goals

- Stop depending on a tracked `local` symlink for builds, VM state, or CI.
- Use normal in-tree Cargo output under `target/`.
- Store kernel-module build output under `build/<kernel-version>/`.
- Store VM state under `vm/`.
- Update helper paths (`yolo load`, bench helpers, CI cache paths) to the new
  layout.

## Non-goals

- No backwards-compatibility shim for the old `local/target` Cargo path.
- No changes to benchmark workload behavior beyond path updates.

## Plan

1. Update docs to describe the new path layout:
   - Cargo artifacts in `target/`
   - kernel-module output in `build/<kernel-version>/`
   - VM state in `vm/`
2. Remove the tracked `local` symlink dependency:
   - drop Cargo `target-dir = "local/target"`
   - point Makefile install/build outputs at the real paths
   - point `vm.py` at `vm/`
3. Update helper code and scripts:
   - `user/cmd/load.rs`
   - `bench/Makefile`
   - `bench/run.sh`
   - `.github/workflows/ci.yml`
4. Verify with VM-based tests and lightweight config validation.

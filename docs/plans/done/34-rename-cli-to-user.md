# 34 — Rename top-level `cli/` source tree to `user/`

## Problem

The top-level Rust userspace implementation lives under `cli/`, but the desired
internal name for that source tree is `user/`. This rename should affect the
repository layout and internal path references without changing the external
`agfs` CLI or the `tests/cli/` test suite naming.

## Approach

Treat this as a source-layout refactor:

- rename the top-level userspace tree from `cli/` to `user/`,
- update build metadata and scripts that point at that tree,
- update documentation that describes the repository layout or build targets,
- preserve user-facing CLI terminology where it still describes the command-line
  interface rather than the internal source directory.

## Changes

### 1. Documentation

- Update `AGENTS.md` project-structure and build/test references from `cli/` /
  `make cli` to `user/` / `make user`.
- Update `docs/architecture.md` source layout to show `user/`.
- Update any other current docs that talk about the tracked userspace tree in
  benchmark metadata or source-layout terms.
- Leave `docs/cli.md` as the CLI reference because the public interface is still
  the CLI.

### 2. Build metadata and source layout

- Rename the directory `cli/` to `user/`.
- Update `Cargo.toml` lib/bin paths from `cli/*.rs` to `user/*.rs`.
- Rename the Makefile build target from `cli` to `user`, and update dependent
  targets accordingly.

### 3. Internal path-based code references

- Update code that explicitly tracks or reports userspace-tree paths, such as
  benchmark freshness/dirty checks, from `cli/` to `user/`.
- Rename internal variable names where they specifically refer to the top-level
  userspace tree rather than to a parsed command-line interface object.

### 4. Verification

- Run the existing build/test workflow needed to verify the rename.
- Run the required review passes and address any findings.

## Notes

- `tests/cli/` stays named `tests/cli/`.
- User-facing wording like “CLI Reference” or command descriptions should remain
  unchanged unless they incorrectly refer to the internal source tree.

# 43 — Rename checkpoint/restore → snapshot/travel

## Goal

Rename the user-facing time-travel vocabulary across the whole project:

- `checkpoint` → `snapshot` (take a named savepoint)
- `restore` → `travel` (go to a snapshot)
- `timeline` stays — it already fits the time-travel model.

No backward compatibility / aliases are needed (no external users).

## Scope

The kernel/protocol layer names the mechanism **`mark`**, not "checkpoint", so
the on-disk/ioctl format is unaffected. `mark`/`marker` are intentionally left
as-is (a distinct internal term, not part of this mapping).

Touched:

- `user/` — `main.rs` (CLI subcommands `Checkpoint`→`Snapshot`, `Restore`→`Travel`),
  `cmd/*.rs`, `config.rs`, `journal/meta.rs`.
- `kmod/` — two comments only (`yolofs.h`, `dir.c`).
- `tests/` — cli + internals + fs tests and their `mod.rs`.
- Living docs — `architecture.md`, `cli.md`, `staging.md`, `permissions.md`.
- File renames: `cmd/checkpoint.rs`→`cmd/snapshot.rs`, `cmd/restore.rs`→
  `cmd/travel.rs`, and the matching `tests/{cli,internals}/test_checkpoint.rs`→
  `test_snapshot.rs`, `test_restore.rs`→`test_travel.rs`.

**Not touched:** `docs/plans/**` (historical record).

## Morphology

`restore` ends in a silent `e`, `travel` does not, so a blind substring swap
misspells inflections. Apply case-aware rules longest-first:

- `restored`→`traveled`, `restoring`→`traveling`, `restores`→`travels`,
  `restore`→`travel`
- `checkpoints`→`snapshots`, `checkpoint`→`snapshot` (no `-ed`/`-ing` forms exist)

## Verification

- `cargo build --tests` (compiles all rename sites), `cargo test --lib`.
- `make kmod` builds.
- `make test-vm` for the e2e/cli/internals suites (needs the VM).
- Re-read the four living docs for transitive phrasings ("restore X to Y") that
  read awkwardly as "travel" and reword.

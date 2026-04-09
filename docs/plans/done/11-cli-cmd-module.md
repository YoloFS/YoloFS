# 11 — Move subcommand files into `cmd/` module

## Problem

All CLI subcommand files and library modules live side-by-side in `cli/`.
This makes it hard to distinguish subcommands from shared libraries.

## Changes

1. **Rename** `timeline_cmd.rs` → `cmd/timeline.rs`
2. **Rename** `journal_cmd.rs` → `cmd/audit.rs` (command also renamed: `yolofs journal` → `yolo audit`)
3. **Move** all subcommand files into `cli/cmd/`: abort, checkpoint, commit, diff, exec, load, mount, restore, watch
4. **Create** `cli/cmd/mod.rs` with `pub mod` declarations
5. **Update** `cli/lib.rs`: replace individual subcommand mods with `pub mod cmd`
6. **Update** `cli/main.rs`: import from `yolofs::cmd::*`, rename `Command::Journal` → `Command::Audit`
7. **Update** cross-subcommand imports to use `super::` (commit→abort, exec→checkpoint, mount→load/commit/abort, load→mount)
8. **Rename** `kmod.rs` → `load.rs`
8. **Update** tests: rename `test_journal.rs` → `test_audit.rs`, update `["journal"]` → `["audit"]`
9. **Update** docs and example.sh: `yolofs journal` → `yolo audit`

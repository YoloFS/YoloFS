# 80 — Rename `cmd/exec.rs` module to `cmd/run.rs`

Align the module file name with its subcommand (`Command::Run`), matching every
other `cmd/*.rs` (`review.rs`, `commit.rs`, `audit.rs`, …). Pure rename, no
behavior change.

The public entry stays `run()`, so the call becomes `run::run(...)` — the same
`module::run` shape as `commit::run` / `abort::run` / `audit::run`.

The word "exec" that describes the *mechanism* (the unshare / pivot_root /
execvp machinery, `chroot_pre_exec`, "pre-exec hook") stays — the module still
execs; only the file/module name and file-path references change.

## Rename
- `git mv user/cmd/exec.rs user/cmd/run.rs`; update its header comment
  `// yolo CLI — exec.rs` → `run.rs`.
- `user/cmd/mod.rs`: `pub mod exec;` → `pub mod run;`.
- `user/main.rs`: import `exec` → `run`; call sites `exec::run` / `exec::announce`
  / `exec::Snapshot` → `run::…`.

## Filename references (comments/docs)
- `user/cmd/mount.rs:119` comment `see exec.rs` → `see run.rs`.
- `tests/cli/test_run.rs:122` comment `set by exec.rs` → `run.rs`.
- `docs/architecture.md`: source-tree line `exec.rs` → `run.rs`.
- `docs/cli.md`: `` `exec.rs` calls unshare(...) `` → `` `run.rs` ``.

Leave `docs/plans/done/*` (historical) untouched.

## Verification
- `cargo build` / clippy clean; `make test` green.
- Code review per AGENTS.md.

# 79 — Rename the `yolo journal` command to `yolo audit`

Rename the **CLI command** `journal` → `audit`. Keep every use of "journal"
that names the *mechanism/artifact*: the on-disk `.yolofs/journal` file,
`kmod/journal.c`, journal records, and the `yolofs::journal` parsing library
(`user/journal/`). The command is a verb the user runs; the journal is the noun
it reads.

Rationale: every other subcommand is a verb (`review`, `commit`, `abort`,
`snapshot`, `travel`, …) — `journal` was the lone noun. `audit` restores the
verb grammar and sits one level more detailed than `review` (net summary →
`review`; every raw record incl. the G access-trail and C rule-trail →
`audit`).

## Keep unchanged (artifact / mechanism)

- `.yolofs/journal` filename and all paths to it.
- `kmod/journal.c`, journal records (S/D/R/P/T/G/C), journal format docs.
- `user/journal/` library (`Journal`, `Record`, parsing) and its `yolofs::journal` path.
- Paper prose naming the "append-only directory journal" mechanism, and
  "truncates the journal" (abort).

## Rename (the command surface)

### Code
- `user/main.rs`: `Command::Journal` → `Command::Audit`; dispatch arm;
  doc comment; the `print_overview()` History entry `("journal", …)` →
  `("audit", …)`; import list.
- `user/lib.rs`: `AGENT_ALLOWED` entry `"journal"` → `"audit"`.
- `user/cmd/journal.rs` → `user/cmd/audit.rs`; update `user/cmd/mod.rs`
  (`pub mod journal;` → `pub mod audit;`). Function stays `run`; the file's
  header comment and doc strings that say the *command* `yolo journal` become
  `yolo audit`. Its `use crate::journal::{self, Journal};` (library) stays.

### Tests
- `tests/cli/test_journal.rs` → `tests/cli/test_audit.rs`; change subcommand
  invocations `s.cli(&["journal", …])` → `["audit", …]` and command-referring
  comments. Leave `.yolofs/journal` path reads and `yolofs::journal::Journal`
  usage as-is.
- Sweep other `tests/cli/*` for `s.cli(&["journal"…])` invocations (none found
  outside test_journal.rs, but re-grep to be sure) and command-referring
  comments (`yolo journal -- <path>` in test_diff.rs / test_snapshot.rs).

### Docs (docs-first: do these before code)
- `docs/cli.md`: the `$ yolo journal …` examples, the `AGENT_ALLOWED` list, the
  "review and journal share one range grammar" prose, and the agent-allowed
  table row → `audit`.
- `docs/architecture.md`: the `cmd/journal.rs` tree line → `cmd/audit.rs`
  (`yolo audit`).
- `docs/permissions.md`, `docs/staging.md`: prose that says the *command*
  `yolo journal` surfaces G/C records / shares the range grammar → `yolo audit`.
  Keep "the journal" (artifact) references.
- `user/templates/agent-guide.md`: `yolo journal -- <path>` → `yolo audit -- <path>`.

### Paper (separate submodule)
- `figures/audit.tex`: header `\fsname{} journal` → `\fsname{} audit`.
- `sections/51-staging.tex`: the userspace-op `\emph{journal}` and
  "\emph{Journal} displays the recorded actions" → `audit` / `Audit`. Keep
  "truncates the journal".
- `sections/54-impl.tex`: `\texttt{journal}/\texttt{review}` → `\texttt{audit}/\texttt{review}`.

## Follow-on: top-level help reorganization

While renaming, restructure `print_overview()` (bare `yolo`) and reorder the
clap `Command` enum to match:

- Groups, in order: **Setup** (`init`), **Staging** (`run`, `review`, `audit`,
  `commit`, `abort` — `audit` now lives here, next to `review`), **Snapshots**
  (`snapshot`, `travel`, `timeline`, renamed from "History"), **Permissions**
  (`rule`, `watch`), **Advanced** (renamed from "Manual control").
- Advanced collapses the triples to one line each: `mount` (with `unmount` /
  `remount` described inline) and `load` (with `unload` / `reload`), instead of
  six separate rows. All six still exist as real subcommands in `yolo --help`.
- Enum section comments (`// ── … ──`) track the new group names/order. Dispatch
  is name-matched, so reordering is safe.

## Verification

- `make test` (unit + e2e) green; `tests/cli/test_audit.rs` passes.
- `cargo build` clean (no stale `Command::Journal` / `cmd::journal` refs).
- Paper compiles (if building locally).
- Code review per AGENTS.md.

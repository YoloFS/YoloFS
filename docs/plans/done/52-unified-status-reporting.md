# 52 — Unified status reporting (`report` module)

## Problem

The CLI's status/error/debug output is ad-hoc: every command file colors and
phrases its own messages. Today we have at least five competing styles:

- `yolo:` prefix colored cyan/yellow/red with plain text (`mount.rs`, `main.rs`)
- whole lines colored (`"yolo: loading kernel module".cyan()` in `load.rs`)
- bare colored verbs (`"created".green().bold()` in `init.rs`,
  `"rule applied:".cyan().bold()` in `config.rs`)
- full-sentence results on **stdout** (`Committed 1 change.` cyan bold,
  `Staging discarded.` yellow bold, `Traveled to …` cyan bold)
- tagged blocks (`[ask]` yellow bold in `watch.rs`, `warning:` in `config.rs`,
  `Error: {e:?}` in `main.rs`)

Two specific lines also add noise:

- `yolo: command exited with 1` after `yolo run -- <cmd>` — redundant; the exit
  code is propagated and the command printed its own error.
- the `(permission=1,staging=1,prompt_timeout=30)` suffix on the mount message —
  internal mount-option syntax, not user-facing information.

## Design

### One module, one line shape

New module `user/report.rs`. Every status line is:

```
yolo: <message>
```

Only the `yolo:` prefix is colored; the color encodes the status. The message
body is plain. Levels:

| fn          | prefix color | meaning                                                  |
|-------------|--------------|----------------------------------------------------------|
| `info()`    | cyan         | progress / state change underway (`loading kernel module …`, `applying 12 rules …`, `watching for permission requests …`) |
| `success()` | green        | a completed state change (`mounted …`, `created …`, `committed 2 changes`, `rule applied: …`, `snapshot 3 "build"`, `traveled to …`, `staging discarded`) |
| `warn()`    | yellow       | non-fatal problem or attention needed (`skipping rule …`, `snapshot failed: …`, an `ask` request, busy mountpoint) |
| `error()`   | red          | fatal — the command exits non-zero (the anyhow chain from `main`) |
| `hint()`    | dimmed       | guidance and no-ops (`run \`yolo watch\` …`, `nothing to commit`, `already initialized`, `no changes, skipping snapshot`) |

Two helpers complete the vocabulary:

- `detail(msg)` — a two-space-indented, uncolored continuation line under the
  preceding status line (blocking PIDs, the `rule: …` line under an ask).
- `prompt(msg)` — a `yolo:`-prefixed (yellow) inline question without a
  trailing newline, flushed (`discard all staged changes? [y/N]:`).

### Streams

All status goes to **stderr**. stdout is reserved for the data a command was
asked for: review summaries/diffs, timeline/journal listings, `rule
list`/`resolve` output, the bare-`yolo` overview. This moves
`Committed …`/`Staging discarded.`/`Traveled …`/`Nothing to commit.` and the
post-`yolo run` snapshot footer from stdout to stderr.

Empty *data* answers stay on stdout but get one consistent, dimmed,
parenthesized form — they are data ("the list is empty"), not status:
`(no changes staged)`, `(no changes)`, `(no snapshots)`, `(no journal
records)`, `(no rules configured)`. Data captions like
`(latest snapshot · \`yolo review all\` for everything since base)` and the
`N staged changes` count also stay on stdout — they describe the data above
them.

### Removed lines

- `yolo: command exited with N` (exec.rs) — deleted, code still propagated.
- mount-option suffix `(permission=1,…)` on the mount/already-mounted lines.

### Message inventory (old → new)

| file | old | new |
|---|---|---|
| main.rs | `Error: {e:?}` | `error("{e:#}")` (single-line chain) |
| main.rs | `yolo:` cyan ` kernel module already loaded` | `hint("kernel module already loaded")` |
| load.rs | `yolo: loading kernel module <ko>` (all cyan) | `info("loading kernel module <ko>")` |
| load.rs | `yolo:` cyan ` kernel module not loaded` | `hint("kernel module not loaded")` |
| load.rs | `yolo: unloading kernel module` (all cyan) | `info("unloading kernel module")` |
| load.rs | `yolo: unmounting <dir>` (cyan) | `info("unmounting <dir>")` |
| mount.rs | `yolo: mounting <mnt> (<opts>)` before mounting | `success("mounted <mnt>")` after mounting |
| mount.rs | `yolo: mounted at <mnt> (<opts>)` | `hint("already mounted at <mnt>")` |
| mount.rs | `yolo: unmounted <mnt>` (cyan) | `success("unmounted <mnt>")` |
| mount.rs | `yolo:` red ` <mnt> is busy, blocked by:` + `  PID …` | `warn(…)` + `detail("PID …")` |
| mount.rs | `Kill these processes? [y/N]` | `prompt("kill these processes? [y/N]")` |
| mount.rs | `Warning: staged changes will be lost …` yellow bold + bold prompt | `warn("staged changes will be lost (\`yolo review\` to see them)")` + `prompt("[c]ommit, [a]bort, or [q]uit? [default: quit]:")` |
| mount.rs | `yolo:` yellow ` run \`yolo watch\` …` | `hint("run \`yolo watch\` to answer permission prompts")` |
| init.rs | `created` green bold ` <path>` | `success("created <path>")` |
| init.rs | `yolo:` cyan ` already initialized` | `hint("already initialized")` |
| config.rs | `yolo: applying N rule(s) from yolofs.toml` (cyan) | `info("applying N rule(s) from yolofs.toml")` |
| config.rs | `  ✗ <path> = <perm>: <err>` | `warn("skipping rule <path> = <perm>: <err:#>")` |
| config.rs | `rule applied:` cyan bold | `success("rule applied: <path> = <perm>")` |
| config.rs | `rule saved:` dimmed | `success("rule saved: <path> = <perm> (takes effect on next mount)")` |
| config.rs | `no rules configured` dimmed (stderr) | stdout `(no rules configured)` dimmed |
| config.rs | `warning:` yellow bold ` yolofs.toml says …` | `warn("yolofs.toml says …")` |
| exec.rs | `snapshot N` cyan bold (quiet run) | `info("snapshot N")` |
| exec.rs | `yolo: no changes, skipping snapshot` dimmed | `hint("no changes, skipping snapshot")` |
| exec.rs | `yolo: command exited with` red ` N` | *(removed)* |
| exec.rs | `yolo: snapshot failed:` yellow ` <e>` | `warn("snapshot failed: <e:#>")` |
| commit.rs | `Nothing to commit.` yellow (stdout) | `hint("nothing to commit")` |
| commit.rs | `Committed N change(s).` cyan bold (stdout) | `success("committed N change(s)")` |
| abort.rs | `Nothing to discard.` yellow (stdout) | `hint("nothing to discard")` |
| abort.rs | `You have staged changes …` + `Discard them all? [y/N]:` | `prompt("discard all staged changes? (\`yolo review\` to see them) [y/N]:")` |
| abort.rs | `Abort cancelled.` dimmed | `hint("abort cancelled")` |
| abort.rs | `Staging discarded.` yellow bold (stdout) | `success("staging discarded")` |
| travel.rs | `Traveled to <label> (N staged changes).` cyan bold (stdout) | `success("traveled to <label> (N staged change(s))")` |
| snapshot.rs | `snapshot N` cyan bold ` <name>` dimmed | `success("snapshot N \"<name>\"")` |
| watch.rs | `yolo: watching …` (all cyan) | `info("watching for permission requests …")` |
| watch.rs | `[ask]` yellow bold ` <comm> wants to <op> <path>` | `warn("<comm> wants to <op> <path>")` |
| watch.rs | `  rule: <source> <phrase>` | `detail("rule: <source> <phrase>")` |
| watch.rs | `  allow [y]es / [d]eny …` (colored keys) | `prompt("allow [y]es / [d]eny (enter = yes):")` |
| watch.rs | `  unknown: x, denying` / `  → allow (req #2)` | `detail(…)` (unchanged shape) |
| watch.rs | `yolo watch: write error: <e>` | `warn("write error: <e>")` |
| review.rs | `yolo:` cyan + dimmed `N staged change in snapshot G · …` (stdout) | `info(…)` same wording (stderr) |
| review.rs | `No changes staged.` / `No changes in \`spec\`.` / `No changes.` yellow (stdout) | `(no changes staged)` / `(no changes in \`spec\`)` / `(no changes)` dimmed (stdout) |
| timeline.rs | `No snapshots.` yellow | `(no snapshots)` dimmed |
| journal.rs (cmd) | `No journal records.` yellow | `(no journal records)` dimmed |

Data output (review summaries/diffs/notes, timeline and journal rows, `rule
list`/`resolve` rows, the overview) is unchanged.

## Steps

1. Docs: add an "Output and status reporting" section to `docs/cli.md`
   describing the line shape, levels/colors, and the stdout/stderr split.
2. Add `user/report.rs` (with inline unit tests for the rendered line shape:
   prefix-only coloring, level→color mapping) and register it in `lib.rs`.
3. Migrate every call site listed above; delete the two removed lines; drop
   now-unused `colored` imports in files that only emitted status.
4. Update e2e assertions in `tests/cli/` (commit/abort/travel/run/mount/rules/
   watch/status/diff/lifecycle) for new wording, casing, and streams.
5. `make user`, `make lint`, `make test-vm`.
6. Regenerate `example.out` via `example.sh`.
7. Full parallel-sub-agent code review per AGENTS.md; triage findings.

## Non-goals

- No change to *data* rendering (diff colors, timeline dimming, rule listing).
- No change to the forced-color policy (`colored::control::set_override(true)`
  in `main`); the example pipeline relies on it.
- No new debug/verbosity flag — the module gives debug output a home later,
  but nothing emits at a debug level today.

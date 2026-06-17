# 65 — Agent guide scaffolded at `yolo init`

## Problem

`yolo init` scaffolds a pre-tool-use hook that wraps every agent shell command
as `yolo run -- <cmd>`, so effects are staged. But nothing tells the agent what
that means or which `yolo` subcommands it may use to inspect and recover. Today
that knowledge only exists as a hand-maintained prompt inside the agent-eval
harness (`YOLOFS_PROMPT`), which already drifted out of sync with the CLI
(referenced removed `status`/`diff`/`audit`/`restore`/`checkpoint` names).

The CLI already encodes the agent-vs-human policy in one place
(`AGENT_ALLOWED` = `review`, `journal`, `timeline`, `travel`, `snapshot`;
everything else is reserved for the human and default-denied for the agent).
The guide should be generated from, and kept in lockstep with, that list.

## Goal

`yolo init` writes an always-loaded agent guide to each selected agent's native
context file, from a single canonical template, so a freshly-initialized project
teaches the agent how to work under YoloFS with no per-harness prompt.

## Design

- One canonical source: `user/templates/agent-guide.md`. Same bytes written
  under three names depending on the agent's convention:
  - `claude`  → `CLAUDE.md` (project root)
  - `gemini`  → `GEMINI.md` (project root)
  - `copilot` → `AGENTS.md` (project root)
- These are **root-level** files, not under the hook dir, because that is where
  each agent auto-loads project memory. So `init.rs` must support writing a
  template file at a path relative to the project root, not only under a single
  `dir`.
- Idempotent and non-destructive, exactly like the hooks: skip if the target
  already exists (never clobber a user's existing `CLAUDE.md`/`AGENTS.md`).
- Single source of truth for the allow-list: lift `AGENT_ALLOWED` from
  `main.rs` into `lib.rs` (`yolofs::AGENT_ALLOWED`) so the runtime gate
  (`run_agent_yolo`) and a guide drift-guard test both reference it.

## Content of the guide

Tells the agent: commands are staged, not applied; inspect with `yolo review`
/ `review --diff` / `journal -- <path>` / `timeline`; bookmark and rewind with
`yolo snapshot` / `travel`; leave the result staged for a human and do NOT run
`commit`/`abort`/`rule` (the human's, and blocked for the agent anyway). It ends
by listing exactly the `AGENT_ALLOWED` subcommands.

## Steps

1. Docs first: `docs/cli.md` `yolo init` section describes the guide files and
   their shared source / allow-list. (done)
2. Lift `AGENT_ALLOWED` into `lib.rs`; update `main.rs` to use it.
3. Add `user/templates/agent-guide.md`.
4. Refactor `init.rs` `AgentTemplate` to write files at root-relative paths;
   add the guide entry to each agent (`CLAUDE.md`/`GEMINI.md`/`AGENTS.md`).
5. Tests:
   - scaffold writes the guide with the right name per agent; idempotent.
   - drift-guard: the template mentions every `AGENT_ALLOWED` command and does
     not advertise `commit`/`abort`/`rule` as allowed.
   - update existing `init.rs` tests that assume the old `AgentTemplate.dir`
     shape and file counts.
6. Regenerate `example.out` (`./example.sh`) — `init` now also reports a created
   `CLAUDE.md`.
7. `make test-vm`.
8. Full parallel-sub-agent code review per `AGENTS.md`; then move this plan to
   `docs/plans/done/`.

## Out of scope (follow-on, in `agent-eval`)

Point the eval at the scaffolded guide instead of its hardcoded `YOLOFS_PROMPT`,
and fix the stale `agents_dir()` template path (`filesystem/agents` →
`filesystem/user/templates`). Tracked separately so the skill content can be
reviewed first.

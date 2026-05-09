## Why

The current permission model exposes redundant user-facing states
(`allow-rw`, `allow-ro`, `allow-rx`, `hide`) that do not match the paper's
intended rule tree in `paper/sections/44_permission.tex`. The paper describes
five states total: `ask` (default/internal), `allow`, `read-only`, `deny`,
and `hidden`.

## Goals

- Drop `allow-rw`, `allow-ro`, `allow-rx`, and `hide`.
- Replace them with canonical `allow`, `ro`, `deny`, and `hidden`.
- Define `allow` as read + write + execute and `ro` as read + execute with no
  write.
- Keep `ask` as the internal default state and existing mount option /
  ask-protocol mechanism.
- Update CLI, config parsing, kernel permission checks, docs, and tests to the
  new state set.

## Non-goals

- No backwards-compatibility aliases for removed rule names.
- No changes to the ask protocol structure beyond the decision enum cleanup.

## Plan

1. Update docs to describe the new permission states and examples.
2. Collapse the userspace and kernel permission enums to:
   `ASK`, `ALLOW`, `RO`, `DENY`, `HIDDEN`.
3. Update permission enforcement:
   - `allow` permits read/write/execute
   - `ro` permits read/execute and denies write
   - `deny` denies access
   - `hidden` returns `ENOENT`
4. Update defaults, CLI parsing/prompts, and config serialization.
5. Rewrite permission tests to cover the new state set.
6. Verify with VM-based tests if the environment allows the VM to start.

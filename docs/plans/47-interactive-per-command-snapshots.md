# 47 — interactive per-command snapshots (the agent experience for humans)

Make bare `yolo` launch an interactive shell that auto-snapshots **and shows
status after each command**, giving a human the same per-command checkpoint
trail an agent gets (each agent tool-call already runs through its own
`yolo exec -- <cmd>`, so it snapshots per command).

## Why it (mostly) already works

`yolo exec` chroots the child to the mount root (`exec.rs::chroot_pre_exec`),
so a shell launched by bare `yolo` — and its hooks — run **inside** the mount.
But yolofs *stacks over `/`*, so the session storage (`.yolofs/journal`,
`.yolofs/inodes/`) is reachable inside at its absolute path; it was only
**hidden** by the default `.yolofs = hide` rule. With the rules below,
`yolo status`/`diff`/`audit` already work in-mount (verified: `yolo exec -- yolo
status` lists staged changes). The only thing that *doesn't* reach is the `.ctl`
ioctl file — it lives at the mount root (`/.ctl`), not at `<session>/mnt/.ctl`.

## Commit 1 — Default rules: expose storage read-only in-mount (done)

In the built-in `example/yolofs.toml`:

- `.yolofs = read` — staging internals readable (but not writable) from inside.
- `.yolofs/mnt = hide` — hide the nested mountpoint (avoids recursion/confusion).
- `yolofs.toml = read` — config readable from inside.

No bind mount, no kernel change. Read-only means a sandboxed command can inspect
the journal/blobs but can't corrupt staging.

## Commit 2 — CLI: reach `.ctl` from inside the mount

- `ioctl::open` tries `<session>/mnt/.ctl` and, on `NotFound`, **falls back to
  `/.ctl`** (the mount-root control file). Outside the mount the first path
  works; inside, `.yolofs/mnt` is hidden so it falls through to `/.ctl`. No env
  flag, no in-mount detection. (Journal/inodes need *no* change — they resolve
  via the absolute session path through the overlay.)
- No explicit guard on `commit`/`abort` in-mount: the read-only `.yolofs` makes
  their storage writes fail, and from inside the chroot they'd target the
  overlay rather than the real base anyway, so they're already non-functional.
- Add **`yolo snapshot --if-changed`** (`YOLO_SNAPSHOT_IF_CHANGED`) so the
  per-prompt hook never makes empty snapshots on a bare Enter / no-op command.

## Commit 3 — Shell integration (the per-command hook)

- Bare `yolo` (no `--`, interactive) launches **bash** (hardcoded; today it's
  `sh`, which has no post-command hook).
- Set `PROMPT_COMMAND` so that, after each command, it runs
  `yolo snapshot --if-changed "<cmd>"` then `yolo status`, preserving `$?`
  (`__rc=$?; …; (exit $__rc)`); name the snapshot from `fc -ln -1`.
- `yolo -- <cmd>` and `yolo exec -- <cmd>` are unchanged (single command).
- zsh/fish hooks (via a future `yolo shell-init <shell>`) are out of scope here.

## Out of scope / non-goals

- Re-routing each interactive command *through* `yolo exec` (the bash
  `extdebug` + `DEBUG`-trap "cancel and re-run" hack) — fragile. Snapshot-after
  is robust and gives the same checkpoints.
- Per-command **diff** — too noisy interactively. `status` only; `diff`/`audit`
  on demand (they also work in-mount now).

## Risks / decisions

- The `ioctl::open` fallback fires only on `NotFound` (not on other errors), so
  it won't mask a real ioctl/permission failure.
- **Exposure:** `.yolofs = read` exposes the journal + staged blobs (read-only)
  to *every* sandboxed command, not just the interactive shell — the user's own
  data, but a behavior change for agents (they could previously not see it).
  Acceptable per the new default; note it in docs/permissions.md.
- `/.ctl` is already reachable in-mount today, so a sandboxed command can issue
  control ioctls (snapshot/travel/rule-set). This plan relies on that for
  snapshot; worth a separate look at whether `.ctl` ops should require privilege.
- No in-mount guard on `commit`/`abort`: a manual `yolo commit` typed inside the
  shell fails on the read-only storage rather than refusing cleanly. Acceptable
  for now; revisit if the messy failure becomes a footgun.
- `PROMPT_COMMAND` clobbers `$?` unless saved/restored; the hook must round-trip
  the exit status.

## Validation

- Verified: `yolo exec -- yolo status` works in-mount with the rules above;
  `yolo exec -- yolo snapshot` fails on `.ctl` (motivates commit 2's fallback).
- VM e2e: assert `yolo exec -- yolo snapshot --if-changed` works (reaches
  `/.ctl` via the fallback).
- VM e2e: drive a bash hook non-interactively; assert one snapshot per changing
  command, none for no-ops.
- Unit: `ioctl::open` falls back to `/.ctl`; `--if-changed` skips when unchanged.

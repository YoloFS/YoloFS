# 54 — `yolo run` auto-mount + orthogonal lifecycle

Depends on plan 57 (`RESTORE` ioctl + mount-time replay).

## Problem

The normal workflow currently requires an explicit `yolo mount` before the
first `yolo run`, even though mounting and module loading are safe,
idempotent prerequisites of running a command.

The CLI also conflates the live kernel view with the durable `.yolofs/`
artifact. Those are independent resources and their commands should remain
orthogonal.

## Design

### Orthogonal commands

- `mount`, `unmount`, and `remount` manage only the live kernel view.
  `unmount` always preserves `.yolofs/`; it never prompts, commits, aborts,
  or removes the artifact. `remount` is view teardown followed by mount.
- `commit` and `abort` are artifact disposition commands. They work with or
  without a live view and never mount, unmount, or remove `.yolofs/`. When a
  live view exists, they first restore it to base so open staging fds can
  reject the operation before any artifact or base changes happen.
- `.yolofs/` may persist without a live view, including when empty. There is
  no first-class stale-session state or stale-session cleanup behavior.
- `unload` tears down live views while preserving their artifacts.

Artifact removal is a separate concern and is not part of this plan.

### `yolo run` mounts on demand

`exec::run` calls `mount::ensure_mounted()` before entering the overlay:

1. If `.yolofs/mnt` is already a live mountpoint, return it without output.
2. If the cwd contains `yolofs.toml`, announce the implicit mount and call
   `mount::mount()`.
3. Otherwise, fail with:
   `not a yolofs project — run \`yolo init\` first (or \`yolo mount\` to mount anyway)`.

`.yolofs/` is the durable artifact, not the project marker for auto-run. A
directory is a YoloFS project when it has `yolofs.toml`; whether its live view
is mounted is tracked through `.yolofs/mnt`. Only the overlay-exec path
auto-mounts. Host-side commands and the agent-subcommand path do not.

The implicit mount announces itself before mounting:

```text
yolo: not mounted — mounting now (run `yolo unmount` when you're done)
```

Normal mount output follows. A second run on the live view emits no mount
announcement.

### Restore existing artifacts

Mount distinguishes a newly created artifact from an existing artifact.
When an artifact already exists, mount always calls `RESTORE` after creating
the live view, including when the serialized current tree is empty. This
restores generation, dirty state, and allocator continuation as well as the
tree itself.

Only an artifact with staged changes emits:

```text
yolo: restored staged changes from a previous session — `yolo review` to inspect
```

If restore fails, mount tears down only the live view and preserves the
artifact. `review`, `commit`, and `abort` remain usable without mounting.

While unmounted, the base can change beneath path-based rename redirects. A
missing redirect source therefore may make a later restore fail; this is
reported without changing the artifact.

### Mount-independent artifact commands

`review`, `commit`, and `abort` operate directly on `.yolofs/` without
requiring a live mount. When a live view exists, commit or abort first sends
`RESTORE(empty)` as the open-fd gate. It then applies/clears the durable
artifact. The empty `.yolofs/` layout remains.

Commands that operate on kernel state (`snapshot`, `travel`, `rule`, and
`watch`) still require a live view.

### `--quiet` becomes `--no-review`

Rename `yolo run --quiet` / `-q` to long-only `--no-review`. It skips only
the post-run review summary and does not suppress mount announcements.

### Workflow-first command order

The bare overview and clap command order use the same groups:

- **Workflow** — `init`, `run`, `review`, `commit`, `abort`
- **Permissions** — `rule`, `watch`
- **History** — `snapshot`, `travel`, `timeline`, `journal`
- **Manual control** — `mount`, `unmount`, `remount`, `load`, `unload`,
  `reload`

Every command remains visible.

### Docs and example

- Update `docs/cli.md`, `docs/staging.md`, `docs/architecture.md`, and
  `docs/permissions.md` for auto-mount, restore, and the orthogonal lifecycle.
- Reword `utils::session_dir()` so its error does not prescribe mounting.
- Drop the explicit mount from `example.sh` and regenerate `example.out`.

## Steps

1. Update docs and this plan.
2. Add `mount::ensure_mounted()` and wire the overlay-exec path to it.
3. Make mount replay every existing artifact through plan 57.
4. Make unmount/remount/unload view-only and commit/abort artifact-disposition
   commands.
5. Rename `--quiet` / `-q` to `--no-review`.
6. Regroup the overview and command enum.
7. Add tests for auto-mount, repeated run, non-project failure, view-only
   unmount/remount, artifact-disposition commit/abort, and restore behavior.
8. Update the example and run `make test-vm`.
9. Run the full parallel code review required by `AGENTS.md`.

## Non-goals

- No artifact deletion command.
- No first-class stale-session cleanup.
- No auto-mount for host-side commands.
- No auto-unmount.
- No locking for concurrent first-run mounts.

# CLI Reference

The CLI communicates with the kernel module via ioctls on a directory fd in the
mount (the mount root, or `.` from inside the mount).
For protocol and travel/snapshot details, see `docs/architecture.md` and
`docs/staging.md`.

## Commands

**Setup**

```bash
$ yolo init              # create yolofs.toml + hooks for every supported agent
$ yolo init --agents claude gemini   # scaffold only the named agent hooks
$ yolo load              # load the kernel module
$ yolo unload            # unmount all sessions and unload the kernel module
$ yolo reload            # unload then reload the kernel module
```

`yolo init` always writes a default `yolofs.toml` (skipped if one exists) and can
scaffold pre-tool-use hook templates that wrap an agent's shell commands so they
run through yolofs. Supported agents: `claude` (`.claude/`), `gemini` (`.gemini/`),
`copilot` (`.github/hooks/`). Bare `yolo init` scaffolds all of them; pass
`--agents <name>...` (repeatable) to scaffold only specific ones. Existing hook
files are never overwritten.

`yolo` is a host-side tool — like `docker`, you run it **outside** the mount and
it manages the session. `review`/`commit`/`abort` need the base
filesystem, which only exists outside, so **every `yolo` command refuses to run
inside the mount**. Stage your work with `yolo run -- <cmd>` and review/commit from
outside. Bare `yolo` prints this command list.

**Session management** — manual control over each step:

```bash
$ yolo mount             # create .yolofs/ layout and mount (auto-loads kmod if needed)
$ yolo run -- make build     # run one command in the staging overlay, then review what it changed
$ yolo run -q -- make build  # same, but quiet — skip the review summary (auto-snapshots after)
$ yolo review            # staged changes for the latest snapshot (summary; default)
$ yolo review --diff     # the same changes as a git-style diff
$ yolo review all        # everything since base (`all` == `..` == `0..`)
$ yolo review --each     # break a range out into one stanza per snapshot
$ yolo review <id>       # snapshot <id>'s own change (the segment it sealed)
$ yolo review <a>..<b>   # changes between two snapshots
$ yolo review <id> --diff -- <path>   # diff one file within a range
$ yolo commit            # apply staged changes to base
$ yolo abort             # discard staged changes (prompts; `--force`/`-f` skips)
$ yolo unmount           # tear down session (prompts if staged changes exist; `-f` skips)
$ yolo remount           # unmount then remount (prompts if staged changes exist; `-f` skips)
```

**Snapshots:**

```bash
$ yolo snapshot              # snapshot with timestamp as name
$ yolo snapshot "my label"   # snapshot with explicit name
$ yolo travel <name|gen>      # travel to a previous snapshot or travel point
$ yolo timeline                # show snapshot/travel DAG (unreachable dimmed)
$ yolo journal               # raw journal records for the latest snapshot (default)
$ yolo journal all           # the entire journal (unreachable dimmed)
$ yolo journal <a>..<b>      # records over a range (review's grammar)
$ yolo journal -- /src/main.rs   # trace operations on a specific file
```

`review` and `journal` share one positional range grammar: a bare `<id>` is
that snapshot's own change, `<a>..<b>` is the span between two (an empty end
means base or tip), and `all` (== `..` == `0..`) is everything since base. Ids
are generation numbers — `0` is the base — and only address live snapshots and
travels, not the unreachable ones a travel leaves behind. A `--diff` path
filter is passed after `--`, so the positional is unambiguously a range.

`yolo timeline` shows the snapshot/travel DAG with unreachable branches
dimmed. Example `yolo timeline` output:

```
snapshot 1 after make build
snapshot 2 after make test
travel   3 → 1
snapshot 4 after make fix
```

Traveling to an earlier point (`yolo travel 3` above) is valid — any
gen_id is a valid travel target. Only entries between 3 and the
new travel become unreachable, preserving earlier history.

**Permission rules and diagnostics:**

```bash
$ yolo rule allow src    # set a rule (verb names the level)
$ yolo rule write-ask /etc  # allow reads, ask before writes
$ yolo rule read-only /usr
$ yolo rule deny /etc
$ yolo rule hide ~/.ssh
$ yolo rule ask /etc/hosts   # force a prompt, overriding an inherited rule
$ yolo rule unset src    # remove a rule (revert to inherited)
$ yolo rule list         # list configured rules (bare `yolo rule` prints the subcommands)
$ yolo rule resolve src  # effective level for a path + where it comes from
$ yolo watch             # handle ask requests (daemon mode)
$ yolo watch --allow-all # answer every ask with "allow" (non-interactive)
```

## Options

Configured via top-level keys in `yolofs.toml`:

| Option | Default | Description |
|---|---|---|
| `permission` | true | Enable permission gating |
| `staging` | true | Enable staging area |
| `auto_snapshot` | true | Auto-snapshot after each command run through yolofs (`yolo run -- <cmd>`), skipped when no changes |
| `prompt_timeout` | 30 | Seconds to wait for an `ask` answer before denying (`0` = wait forever; an unanswered ask is a deny) |

## Execution Environment

Inside the launched shell or command, YoloFS `chroot`s into `.yolofs/mnt` so
that the mounted view becomes `/`. The working directory remains the
caller's original CWD. For example, launching from `/home/user/project`
chroots into `.yolofs/mnt` and sets the working directory to
`/home/user/project` — same absolute path, but now resolved through the
YoloFS mount. That is why runtime examples use absolute paths like `/src` and
`/etc` even when a rule was added as the relative path `src` from the
session root. Files under that session root are typically ruled `allow`;
everything else defaults to `ask`.

## Privilege Model

The YoloFS binary is installed with file capabilities, not setuid root:

```
install -m 0755 … /usr/local/bin/yolo
setcap cap_sys_admin,cap_sys_chroot,cap_sys_module+ep /usr/local/bin/yolo
```

- `cap_sys_admin` — `mount()`, `umount()`, bind-mounting `/proc` `/sys` `/dev`
- `cap_sys_chroot` — `chroot()` into the session mountpoint when running `yolo run -- <cmd>`
- `cap_sys_module` — `finit_module()` / `delete_module()` for `load` / `unload`

The binary therefore runs as the **invoking user** (euid = real uid) with just
these capabilities, never as root. Everything it creates — `.yolofs/`, the
inode store, the journal, the runtime mountpoint, committed files, `init`
scaffolding — is owned by that user automatically, so there is no chown-back
step.

> **Note:** `cap_sys_module` lets the binary load *any* kernel module, which is
> effectively root-equivalent. It replaces the previous `sudo insmod`/`rmmod`.
> If you'd rather keep module loading behind an audited, policy-gated elevation,
> drop `cap_sys_module` from the `setcap` line and have `load`/`unload` shell out
> to `sudo` instead.

### Command execution lifecycle

The `pre_exec` hook in `exec.rs` runs in the child (after fork, before execvp):

1. `chroot()` into `.yolofs/mnt` so the mounted view becomes `/` (uses `cap_sys_chroot`).
2. `chdir()` back to the caller's original working directory.

No uid/gid drop is needed — the process is already the invoking user. `execve`
then clears both capabilities for the spawned command (a non-setuid image with
no file caps, run by a non-root euid, receives an empty capability set), so the
command itself can neither `chroot` nor `mount`.

### Capabilities by command

| Command | Capability | Notes |
|---|---|---|
| `mount`, `unmount`, `remount` | `cap_sys_admin` | `mount()` / `umount()` / bind-mounts |
| `yolo run -- <cmd>` | `cap_sys_chroot` | cleared for the spawned command |
| `load`, `unload`, `reload` | `cap_sys_module` | `finit_module()` / `delete_module()` |
| `rule`, `watch`, `commit`, `abort`, `snapshot`, `travel`, `review`, `journal`, `timeline`, `init` | none | run unprivileged as the user; ioctls go to a dir fd on the mount root |

Because `commit` runs as the user (no `CAP_DAC_OVERRIDE`), it applies staged
changes only to paths the user can write — the normal project workflow. A
staged change to a path the user doesn't own (e.g. a write-ask-approved `/etc`
edit) fails on commit with `EACCES` rather than being written as root.

### Staging blob ownership

The kernel module creates staging blobs via `vfs_create` / `vfs_mkdir` using
`current_cred()`. Both the user's commands inside `yolo run -- <cmd>` and the host-side
CLI run with the invoking user's credentials, so staging blobs — and committed
files — are owned by the real user.

## TTY / Terminal Ownership

When YoloFS runs the default workflow (`yolo` with no subcommand), a
background watch thread handles interactive permission prompts by reading
from the terminal. While the child shell is running, its process group
typically becomes the terminal foreground group. Without special handling
the parent's watch thread would receive `SIGTTIN` when it tries to read
from the terminal, stopping the entire process.

To avoid this, `watch.rs` temporarily claims terminal ownership around each
permission prompt:

1. Save the current foreground process group (`tcgetpgrp`).
2. Ignore `SIGTTIN`/`SIGTTOU` so `tcsetpgrp` won't be stopped.
3. Call `tcsetpgrp` to make the watch thread's process group the
   foreground group.
4. Restore `SIGTTIN`/`SIGTTOU` to default — we are now the foreground
   group so they won't fire.
5. Print the prompt and read the user's answer from stdin.
6. Give the terminal back to the saved foreground group.

# CLI Reference

The CLI communicates with the kernel module via ioctls on `.yolofs/mnt/.ctl`.
For protocol and restore/checkpoint details, see `docs/architecture.md` and
`docs/staging.md`.

## Commands

**Setup**

```bash
$ yolo init              # create a default yolofs.toml in the current directory
$ yolo load              # load the kernel module
$ yolo unload            # unmount all sessions and unload the kernel module
$ yolo reload            # unload then reload the kernel module
```

**Full workflow** — mount, watch, exec, diff, and prompt to commit/abort in one command:

```bash
$ yolo                   # launch sh inside the sandbox
$ yolo -- make build     # run a specific command instead of sh
```

**Session management** — manual control over each step:

```bash
$ yolo mount             # create .yolofs/ layout and mount (auto-loads kmod if needed)
$ yolo exec              # chroot $SHELL into .yolofs/mnt (requires existing mount)
$ yolo exec -- make build
$ yolo status            # show staged changes (grouped by checkpoint when present)
$ yolo status --at <name|gen>           # show single checkpoint segment
$ yolo status --from <name|gen>        # show changes since checkpoint
$ yolo status --to <name|gen>          # show changes up to checkpoint
$ yolo status --from <A> --to <B>      # show changes between two checkpoints
$ yolo diff              # git-style diff of staged vs base (grouped by checkpoint)
$ yolo diff <path>       # diff a single file
$ yolo diff --at <name|gen>             # diff single checkpoint segment
$ yolo diff --from <name|gen>          # diff changes since checkpoint
$ yolo diff --to <name|gen>            # diff changes up to checkpoint
$ yolo diff --from <A> --to <B>        # diff changes between two checkpoints
$ yolo diff --from <name|gen> <path>   # diff a single file since checkpoint
$ yolo commit            # apply staged changes to base
$ yolo abort             # discard staged changes (prompts for confirmation)
$ yolo unmount           # tear down session (prompts if staged changes exist)
$ yolo remount           # unmount then remount (prompts if staged changes exist)
```

**Checkpoints:**

```bash
$ yolo checkpoint              # checkpoint with timestamp as name
$ yolo checkpoint "my label"   # checkpoint with explicit name
$ yolo restore <name|gen>      # restore to a previous checkpoint or restore point
$ yolo timeline                # show checkpoint/restore DAG (unreachable dimmed)
$ yolo audit                 # show every raw journal record (unreachable dimmed)
$ yolo audit --path /src/main.rs  # trace operations on a specific file
```

The `--at`, `--from`, and `--to` flags accept a checkpoint name or
generation number (of any type) and only address live checkpoints and restores
(not unreachable ones created by restores).

`yolo timeline` shows the checkpoint/restore DAG with unreachable branches
dimmed. Example `yolo timeline` output:

```
checkpoint [1] after make build
checkpoint [2] after make test
restore    [3] restored to [1]
checkpoint [4] after make fix
```

Restoring to a restore point (`yolo restore 3` above) is valid — any
gen_id is a valid restore target. Only entries between [3] and the
new restore become unreachable, preserving earlier history.

**Permission rules and diagnostics:**

```bash
$ yolo rule add src allow-rw
$ yolo rule remove src
$ yolo watch             # handle ask requests (daemon mode)
```

## Options

Configured via top-level keys in `yolofs.toml`:

| Option | Default | Description |
|---|---|---|
| `ask_timeout` | 0 (infinite) | Seconds before ask request times out |
| `ask_default` | `deny` | Fallback when no daemon is connected or on timeout |
| `permission` | true | Enable permission gating |
| `staging` | true | Enable staging area |
| `checkpoint` | true | Auto-checkpoint after each `yolo exec` invocation (skipped when no changes) |

## Execution Environment

Inside the launched shell or command, YoloFS `chroot`s into `.yolofs/mnt` so
that the mounted view becomes `/`. The working directory remains the
caller's original CWD. For example, launching from `/home/user/project`
chroots into `.yolofs/mnt` and sets the working directory to
`/home/user/project` — same absolute path, but now resolved through the
YoloFS mount. That is why runtime examples use absolute paths like `/src` and
`/etc` even when a rule was added as the relative path `src` from the
session root. Files under that session root are typically ruled `allow-rw`;
everything else defaults to `ask`.

## Privilege Model

The YoloFS binary is installed setuid root (`install -m 4755 -o root`).
This is needed because `mount()`, `umount()`, bind-mounting `/proc` `/sys`
`/dev`, and `chroot()` all require `CAP_SYS_ADMIN`.

### Privilege lifecycle

The `pre_exec` hook in `exec.rs` performs three steps in the child process
(after fork, before execvp):

1. `chroot()` into `.yolofs/mnt` — needs euid=0.
2. `chdir()` to the caller's original working directory.
3. Permanently drop privileges: `setgid(real_gid)` then `setuid(real_uid)`.

Order matters in step 3 — `setuid()` is irreversible and removes the
ability to call `setgid()`, so gid must be set first.

After the drop, the user's command runs with the invoking user's uid and
gid. The kernel module enforces file access via its rule engine based on
process credentials, not euid.

| Phase | euid | Why |
|---|---|---|
| `mount()`, bind-mounts, `chroot()` | 0 | Require `CAP_SYS_ADMIN` |
| `exec` user command | real uid | User code must not run as root |
| `commit`, `restore`, `status`, `diff` | 0 | Need root for ioctl on the mount |
| `load`/`unload` | delegates to `sudo` | Already handled correctly |

### `.yolofs/` directory ownership

`setup_yolo_dir` creates `.yolofs/`, `inodes/`, `mnt/`, and `journal` as
root (euid=0 from setuid), then `chown`s them all to the real user.
This ensures the user can write staging blobs into `inodes/` and append
to `journal` after the exec privilege drop. Without the chown, the
caller's umask (e.g. 022) would leave `inodes/` as root-owned 0755,
blocking non-root writes.

### Staging blob ownership

The kernel module creates staging blobs via `vfs_create` / `vfs_mkdir`
using `current_cred()`. Since user commands inside `yolo exec` run with
the invoking user's credentials (after the privilege drop), staging blobs
are owned by the real user.

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

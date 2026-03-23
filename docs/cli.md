# CLI Reference

The CLI communicates with the kernel module via ioctls on `.agfs/mnt/.ctl`.
See [Kernel Reference — Control Interface](internals.md#control-interface-ioctl)
for the protocol details.

## Commands

**Setup**

```bash
$ agfs init              # create a default agfs.toml in the current directory
$ agfs load              # load the kernel module
$ agfs unload            # unmount all sessions and unload the kernel module
$ agfs reload            # unload then reload the kernel module
```

**Full workflow** — mount, watch, exec, diff, and prompt to commit/abort in one command:

```bash
$ agfs                   # launch sh inside the sandbox
$ agfs -- make build     # run a specific command instead of sh
```

**Session management** — manual control over each step:

```bash
$ agfs mount             # create .agfs/ layout and mount (auto-loads kmod if needed)
$ agfs exec              # chroot $SHELL into .agfs/mnt (requires existing mount)
$ agfs exec -- make build
$ agfs status            # show staged changes (grouped by checkpoint when present)
$ agfs status --at <name|gen>           # show single checkpoint segment
$ agfs status --from <name|gen>        # show changes since checkpoint
$ agfs status --to <name|gen>          # show changes up to checkpoint
$ agfs status --from <A> --to <B>      # show changes between two checkpoints
$ agfs diff              # git-style diff of staged vs base (grouped by checkpoint)
$ agfs diff <path>       # diff a single file
$ agfs diff --at <name|gen>             # diff single checkpoint segment
$ agfs diff --from <name|gen>          # diff changes since checkpoint
$ agfs diff --to <name|gen>            # diff changes up to checkpoint
$ agfs diff --from <A> --to <B>        # diff changes between two checkpoints
$ agfs diff --from <name|gen> <path>   # diff a single file since checkpoint
$ agfs commit            # apply staged changes to base
$ agfs abort             # discard staged changes (prompts for confirmation)
$ agfs unmount           # tear down session (prompts if staged changes exist)
$ agfs remount           # unmount then remount (prompts if staged changes exist)
```

**Checkpoints:**

```bash
$ agfs checkpoint              # checkpoint with timestamp as name
$ agfs checkpoint "my label"   # checkpoint with explicit name
$ agfs restore <name|gen>       # restore to a previous checkpoint (discards later changes)
$ agfs timeline                # show checkpoint/restore DAG (unreachable dimmed)
$ agfs audit                 # show every raw journal record (unreachable dimmed)
$ agfs audit --path /src/main.rs  # trace operations on a specific file
```

The `--at`, `--from`, and `--to` flags accept a checkpoint name or
generation number and only address live checkpoints (not those in dead
zones created by restores).

`agfs timeline` shows the checkpoint/restore DAG with unreachable branches
dimmed. Example `agfs timeline` output:

```
checkpoint [1] after make build
checkpoint [2] after make test
restore    [3] restored to [1]
checkpoint [4] after make fix
```

**Permission rules and diagnostics:**

```bash
$ agfs rule add src allow-rw
$ agfs rule remove src
$ agfs watch             # handle ask requests (daemon mode)
```

## Options

Configured via top-level keys in `agfs.toml`:

| Option | Default | Description |
|---|---|---|
| `ask_timeout` | 0 (infinite) | Seconds before ask request times out |
| `ask_default` | `deny` | Fallback when no daemon is connected or on timeout |
| `permission` | true | Enable permission gating |
| `staging` | true | Enable staging area |
| `checkpoint` | true | Auto-checkpoint after each `agfs exec` invocation (skipped when no changes) |

## Execution Environment

Inside the launched shell or command, AgFS `chroot`s into `.agfs/mnt` so
that the mounted view becomes `/`. The working directory remains the
caller's original CWD. For example, launching from `/home/user/project`
chroots into `.agfs/mnt` and sets the working directory to
`/home/user/project` — same absolute path, but now resolved through the
AgFS mount. That is why runtime examples use absolute paths like `/src` and
`/etc` even when a rule was added as the relative path `src` from the
session root. Files under that session root are typically ruled `allow-rw`;
everything else defaults to `ask`.

## Privilege Model

The agfs binary is installed setuid root (`install -m 4755 -o root`).
This is needed because `mount()`, `umount()`, bind-mounting `/proc` `/sys`
`/dev`, and `chroot()` all require `CAP_SYS_ADMIN`.

### Privilege lifecycle

The `pre_exec` hook in `exec.rs` performs three steps in the child process
(after fork, before execvp):

1. `chroot()` into `.agfs/mnt` — needs euid=0.
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

### `.agfs/` directory ownership

`setup_agfs_dir` creates `.agfs/`, `inodes/`, `mnt/`, and `journal` as
root (euid=0 from setuid), then `chown`s them all to the real user.
This ensures the user can write staging blobs into `inodes/` and append
to `journal` after the exec privilege drop. Without the chown, the
caller's umask (e.g. 022) would leave `inodes/` as root-owned 0755,
blocking non-root writes.

### Staging blob ownership

The kernel module creates staging blobs via `vfs_create` / `vfs_mkdir`
using `current_cred()`. Since user commands inside `agfs exec` run with
the invoking user's credentials (after the privilege drop), staging blobs
are owned by the real user.

## TTY / Terminal Ownership

When AgFS runs the default workflow (`agfs` with no subcommand), a
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

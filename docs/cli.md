# CLI Reference

The CLI communicates with the kernel module via ioctls on `.agfs/mnt`.
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
$ agfs status --at <name|id> # show state at a checkpoint
$ agfs diff              # git-style diff of staged vs base (grouped by checkpoint)
$ agfs diff --from <name|id> # diff changes since checkpoint
$ agfs commit            # apply staged changes to base
$ agfs commit --at <name|id> # commit only changes up to a checkpoint
$ agfs abort             # discard staged changes (prompts for confirmation)
$ agfs unmount           # tear down session (prompts if staged changes exist)
$ agfs remount           # unmount then remount (prompts if staged changes exist)
```

**Checkpoints:**

```bash
$ agfs checkpoint              # checkpoint with timestamp as name
$ agfs checkpoint "checkpoint" # checkpoint with explicit name
$ agfs log                   # show checkpoint log with change counts
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
| `checkpoint` | true | Auto-checkpoint after each `agfs exec` invocation |

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
| `commit`, `status`, `diff` | 0 | May need access to root-owned staging blobs (see below) |
| `load`/`unload` | delegates to `sudo` | Already handled correctly |

### Staging blob ownership

The kernel module creates staging blobs via `vfs_create` / `vfs_mkdir`
using `current_cred()`. Before the privilege drop fix, `current_cred()`
had euid=0 (from the setuid binary), making all staging blobs root-owned.

The privilege drop in `exec` fixes this for the CLI path: user commands
now run with the real user's credentials, so `current_cred()` in the
kernel sees the invoking user's uid and staging blobs are created with
correct ownership.

Note: `mount` still runs as root (needed for `mount()` syscall), so the
initial `.agfs/` directory structure is root-owned. This is expected —
only the inodes created by user commands inside `agfs exec` need user
ownership.

There is a separate kernel-side issue: `agfs_permission` delegates
directory permission checks to the lower filesystem instead of using
agfs rules (see `inode.c`). This means directory access depends on the
blob's Unix ownership rather than the agfs rule engine. The CLI privilege
drop makes this work in practice (blobs are user-owned), but the kernel
behavior is still incorrect — a direct `mount -t agfs` without the CLI
would still produce root-owned blobs.

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

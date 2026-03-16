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

### Current behavior (BUG)

The binary runs with euid=0 throughout its entire lifetime. It never drops
privileges before exec'ing user commands. This means:

- `agfs exec -- make build` runs `make` with euid=0. Build tools see
  themselves as root and may behave differently (skip permission checks,
  install to system paths, etc).
- Files created by the user's command are visible to the kernel module with
  the process's real uid (1000), but the effective uid (0) can mask
  permission bugs — operations that should fail with EACCES succeed because
  euid is root.
- The staging layer creates blobs under `override_creds(sbi->creator_cred)`,
  where `creator_cred` captures uid=0 from the setuid binary. This makes
  staged directories root-owned, causing EACCES for the real user when
  `agfs_permission` delegates directory checks to the lower filesystem
  (see `tests/perm/test_dir_ops.rs::non_root_mkdir_then_write_inside`).

### Target behavior

The CLI should hold root only for privileged syscalls and drop before
running user code:

| Phase | euid | Why |
|---|---|---|
| `mount()`, bind-mounts, `chroot()` | 0 | Require `CAP_SYS_ADMIN` |
| `exec` user command | real uid | User code must not run as root |
| `commit`, `status`, `diff` | real uid | Operate on user-owned files; kernel module checks real uid |
| `load`/`unload` | delegates to `sudo` | Already handled correctly |

The drop in `exec` should be permanent (`setgid(real_gid)` then
`setuid(real_uid)` — order matters because you cannot change gid after
dropping uid). This goes in the `pre_exec` closure after `chroot()` and
`chdir()`, before the child calls `execvp()`.

Once the staging ownership bug is fixed (staged blobs owned by the real
user, not root), the same drop can be applied to `commit` and other
subcommands that don't need privileged syscalls.

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

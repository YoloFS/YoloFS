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
$ agfs exec              # join namespace, pivot into .agfs/mnt, run $SHELL
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
$ agfs watch             # handle ask requests (runs inside mount daemon)
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

Inside the launched shell or command, AgFS enters a mount namespace and
uses `pivot_root` to make `.agfs/mnt` the new root. The working directory
remains the caller's original CWD. For example, launching from
`/home/user/project` pivots into `.agfs/mnt` and sets the working
directory to `/home/user/project` — same absolute path, but now resolved
through the AgFS mount. That is why runtime examples use absolute paths
like `/src` and `/etc` even when a rule was added as the relative path
`src` from the session root. Files under that session root are typically
ruled `allow-rw`; everything else defaults to `ask`.

**Why `pivot_root` instead of `chroot`**: `chroot(2)` does not provide
any security isolation — the man page explicitly states it "is not
intended to be used for any kind of security purpose". A root process
(or one with `CAP_SYS_CHROOT`) can trivially escape a chroot via
`fchdir` to an open fd outside the root, or via `chroot("../..")`.
`pivot_root` inside a mount namespace replaces the root mount entirely;
after unmounting the old root there is no dentry path back to the host
filesystem. This also eliminates unnecessary VFS path-walk overhead:
with chroot the kernel still resolves the full host path to reach the
mount point, while with pivot_root path resolution starts directly at
the agfs mount root.

## Privilege Model

AgFS runs entirely unprivileged. No setuid binary, no root access
required for normal operation. The agfs kernel module sets
`FS_USERNS_MOUNT` so it can be mounted inside a user namespace.
Unprivileged user namespaces (`CLONE_NEWUSER | CLONE_NEWNS`) are
available on modern kernels and enabled by default on Ubuntu, Fedora,
and Arch.

The only operation requiring real root is loading/unloading the kernel
module (`agfs load` / `agfs unload`), which delegates to `sudo insmod`
/ `sudo rmmod` internally.

### `agfs mount` — namespace daemon

`agfs mount` creates the namespace and stays alive as a daemon,
holding both the namespace and the permission watch loop. Other
commands join the namespace via `setns(2)`.

1. `unshare(CLONE_NEWUSER | CLONE_NEWNS)` — create a user namespace
   (granting `CAP_SYS_ADMIN` inside it) and a private mount namespace.
2. Write uid/gid maps (`/proc/self/uid_map`, `/proc/self/gid_map`) to
   map the real uid/gid 1:1 inside the namespace.
3. `mount("", "/", NULL, MS_PRIVATE|MS_REC, NULL)` — prevent mount
   events from propagating back to the parent namespace.
4. Mount the agfs filesystem on `.agfs/mnt`.
5. Bind-mount `/proc`, `/sys`, `/dev` into `.agfs/mnt`.
6. Write the daemon pid to `.agfs/pid`.
7. Enter the watch loop — handle permission `ask` requests via ioctl
   on `.agfs/mnt`. This is the same logic as `agfs watch` but runs
   inside the mount daemon instead of as a separate process.
8. On `SIGTERM` (from `agfs unmount`), stop the watch loop, unmount,
   and exit.

The namespace, mounts, and watch loop all live in a single daemon
process. If the daemon dies unexpectedly the namespace is cleaned up
by the kernel. `agfs watch` as a standalone command is no longer
needed — it is subsumed by `agfs mount`.

### `agfs exec` — join namespace and pivot

`agfs exec` joins the daemon's namespace and isolates the process via
`pivot_root`:

1. Read the daemon pid from `.agfs/pid`.
2. `setns(open("/proc/<pid>/ns/user"), CLONE_NEWUSER)` — enter the
   daemon's user namespace. This is allowed because the namespace was
   created by the same uid.
3. `setns(open("/proc/<pid>/ns/mnt"), CLONE_NEWNS)` — enter the
   daemon's mount namespace (visible mounts include agfs + bind-mounts).
4. `unshare(CLONE_NEWNS)` — create a private child mount namespace so
   that `pivot_root` does not affect the daemon or other `agfs exec`
   sessions.
5. `chdir(".agfs/mnt")` then `pivot_root(".", ".")` — the agfs mount
   becomes the new root. The old root is stacked underneath.
6. `umount2(".", MNT_DETACH)` — detach the old root. After this there
   is no dentry path back to the host filesystem.
7. `chdir()` to the caller's original working directory.
8. Exec the user command.

No privilege drop needed — the process never had elevated privileges.

### `agfs unmount`

Sends `SIGTERM` to the daemon (pid from `.agfs/pid`). The daemon
stops the watch loop, unmounts the filesystem, and exits, releasing
the namespace.

### Other commands (`commit`, `restore`, `status`, `diff`)

These join the daemon's namespace via `setns` (steps 1-3 of exec)
without `pivot_root`. They issue ioctls on the agfs mount and exit.

| Phase | Privileges | Why |
|---|---|---|
| `load`/`unload` | `sudo` | `insmod`/`rmmod` require real root |
| `mount` (daemon) | user namespace `CAP_SYS_ADMIN` | Unprivileged via `CLONE_NEWUSER` |
| `exec` user command | unprivileged | `setns` into existing namespace |
| `commit`, `restore`, etc. | unprivileged | `setns` + ioctl |

### `.agfs/` directory ownership

`setup_agfs_dir` creates `.agfs/`, `inodes/`, `mnt/`, and `journal`
as the invoking user (no root involved). All files and directories are
owned by the real user from the start.

### Staging blob ownership

The kernel module creates staging blobs via `vfs_create` / `vfs_mkdir`
using `current_cred()`. Since the user namespace maps the real uid 1:1,
staging blobs are owned by the real user.

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

# 70 — Namespace isolation for `yolo run` (pidns + mountns + pivot_root)

## Motivation

`yolo run -- <cmd>` remaps the command's `/` to `.yolofs/mnt` with a single
`chroot()` ([exec.rs](../../user/cmd/exec.rs)). This is load-bearing for the
*permission model*: the kernel gates paths that resolve **through the mount**,
so the root remap is what routes the command into the gated view. A command
that reaches a path *outside* the mount bypasses every rule.

### What is NOT a hole (verified)

The obvious worry — a command does `unshare -Ur` to regain `CAP_SYS_CHROOT`
inside a user namespace and then double-chroots out — **does not work**. The
kernel's `create_user_ns()` calls `current_chrooted()` and returns `EPERM`, so
`unshare(CLONE_NEWUSER)` fails from inside any chroot. Confirmed in-VM: not
chrooted → `unshare(CLONE_NEWUSER)` OK; chrooted → `EPERM`. So the current
`chroot` is *not* vulnerable to that vector, and note the corollary: **switching
to `pivot_root` makes `current_chrooted()` false and re-enables unprivileged
userns creation** for the command — a tradeoff, see below.

### The real hole (verified, confirmed escape)

`/proc` is bind-mounted into the mount ([mount.rs](../../user/cmd/mount.rs)),
and `chroot` leaves the command in the **host mount namespace** sharing the
**host pid namespace**. So `/proc/<pid>/root` of any *ordinary same-uid process
running outside the mount* is a magic symlink that jumps straight to the real
root, bypassing yolofs gating — **no userns required**. Confirmed in-VM: from
inside `yolo run`, `open("/proc/<sleep-pid>/root/<host-file-outside-session>")`
returned the host file's secret contents.

- Privileged neighbours (the parent `yolo`, `init`) are protected by
  `ptrace_may_access` (they hold caps / are root-owned) — those specific pids
  return `EACCES`.
- But any plain same-uid process outside the mount (the agent harness, a shell,
  an editor — ubiquitous in the real threat model) is a usable stepping stone.

`chroot` cannot close this: it shares the host pid namespace, so outside pids
are always visible in `/proc`, and it shares the host mount namespace, so their
root magic-symlinks are followable.

## Design

Run the command in its own **pid namespace** and **mount namespace**, with a
**fresh `/proc`** (so no outside pid is visible) and a `pivot_root` onto
`.yolofs/mnt` with the old host root detached (so no host mount is reachable).

### Namespace creation — why `unshare` must split across parent and child

`unshare(CLONE_NEWPID)` does **not** move the caller into the new pid namespace;
it only places the caller's *future children* there. The `pre_exec` hook runs in
the already-forked child that then `execve`s the command, so `unshare` there
cannot put the *exec'd* command in a new pidns. Therefore the pidns must be
unshared in the **parent** (`yolo`) before the fork — exactly what
`unshare --pid --fork` does. The mount namespace, by contrast, *can* be created
by the child itself. So:

**Parent (`exec.rs::run`, before spawning):**
- `unshare(CLONE_NEWPID)` — the next fork's child becomes PID 1 in a new pid
  namespace. (`cap_sys_admin`, already held.) This only affects subsequent
  forks; `yolo` spawns exactly one command then does ioctls (no fork), so
  polluting the parent's child-pidns is harmless.

**Child (`pre_exec`, PID 1 in the new pidns, caps still present pre-`execve`):**
1. `unshare(CLONE_NEWNS)` — private mount namespace for this command.
2. `mount("", "/", NULL, MS_REC | MS_PRIVATE, NULL)` — private propagation, else
   `pivot_root` returns `EINVAL` and detaches could propagate to the host ns.
3. `mount("proc", mnt/proc, "proc", 0, NULL)` — a **fresh** procfs. Because the
   child is already PID 1 in the new pidns, this `/proc` shows only the new
   namespace's processes; no outside pid exists to reach. This is what closes
   the confirmed hole.
4. Bind `mnt/dev`, `mnt/sys` from the host, recursively (`MS_BIND | MS_REC`) so
   `/dev` carries its `devpts`/`shm` submounts (they expose no pid roots).
5. `chdir(mnt)`, `pivot_root(".", ".")` (runc idiom — needs no `put_old` dir;
   `.yolofs/mnt` is already a real mount point, which `pivot_root` requires).
6. `umount2(".", MNT_DETACH)` — detach the old host root; unreachable now.
7. `chdir(cwd)` — restore the caller's working directory.

`/proc`, `/dev`, `/sys` are mounted **under** `new_root` before the pivot, so
they survive the pivot and the old-root detach.

### Async-signal safety

`pre_exec` runs post-`fork`. Every step is a raw `libc` syscall
(`unshare`, `mount`, `chdir`, `pivot_root` via `libc::syscall(SYS_pivot_root,…)`,
`umount2`) — no allocation, no `panic!`, no `std` fs. All `CString`s (`mnt`,
`cwd`, mount targets/sources/fstypes) are built **before** the fork and moved
into the closure. Any failing step returns `io::Error::last_os_error()` so the
spawn fails loudly rather than running the command un-isolated.

### PID 1 semantics

The command runs as PID 1 in its pidns (like `docker run` without an init, or
`unshare --pid --fork`). Consequences, all acceptable for short-lived
`yolo run` commands: if it exits with live children the kernel SIGKILLs them
(cleanup); orphaned grandchildren reparent to it and, if unreaped, linger as
zombies only until the pidns is destroyed on exit; default signal actions for
un-handled signals don't apply to PID 1 (irrelevant — the ancestor `yolo` can
still `SIGKILL` it). Exit-code propagation is unchanged: `yolo` is the real
parent in the ancestor pidns and `waitpid`s the child normally. An init-shim
(a tiny PID 1 that forks the command and reaps) is a possible follow-on if a
real workload turns out to depend on not being init.

### Kernel gate is unchanged

`yolo_caller_inside()` ([ioctl.c](../../kmod/ioctl.c)) compares
`current->fs->root->d_sb == sb`. `pivot_root` sets `current->fs->root` exactly
as `chroot` did, so the in-mount detection and the refusal of gating-defeating
ioctls from inside keep working with no change.

### Accepted tradeoff: userns surface

After `pivot_root` the process root *is* the mount-ns root, so
`current_chrooted()` is false and the command may create user namespaces again
(which `chroot` categorically blocked). This is acceptable here: with a private
mount namespace, a detached old root, and a fresh `/proc` showing no outside
pids, a userns the command creates has no host mount to reach and no outside
process to inspect. Blocking unprivileged userns per-process would require a
seccomp filter (a denylist, more code) — out of scope; noted as a known
residual surface.

## Userspace changes

- [exec.rs](../../user/cmd/exec.rs): parent `unshare(CLONE_NEWPID)` before
  spawn; rewrite `chroot_pre_exec` → `isolate_pre_exec` implementing steps 1–7.
  Own the `/proc` fresh-mount + `/dev` `/sys` binds here (see relocation below).
- Capabilities: `cap_sys_chroot` is no longer used (the isolation needs
  `cap_sys_admin`, already held). Drop it from the `setcap` line
  ([Makefile:40](../../Makefile)) and the capability table.
- Terminology: comments/messages saying "chroot(ed)" —
  [main.rs](../../user/main.rs), [utils.rs](../../user/utils.rs),
  [ioctl.rs](../../user/ioctl.rs) mnt fallback — reword to "inside the mount" /
  "the command's remapped root". The mechanism they rely on (fs root superblock)
  is unchanged.

### Bind-mount relocation (folded in, not a separate step)

The three pseudo-filesystems move out of `yolo mount` (persistent, host
namespace, needs teardown) and into the per-command child, because full
isolation *requires* it: `/proc` specifically must be a fresh mount in the new
pidns, not a host bind, or the hole reopens. `/dev` and `/sys` move alongside it
for symmetry and to make `.yolofs/mnt` clean in the host namespace. This deletes:

- `unbind_mount_pseudofs` ([mount.rs:139](../../user/cmd/mount.rs)) entirely.
- The pseudo-fs half of `unmount` teardown
  ([mount.rs:157](../../user/cmd/mount.rs)) — only the yolofs mount is unmounted.
- The `EBUSY` / `get_blocking_pids` / `umount_or_prompt` kill-prompt path *for
  the pseudo-fs*; it remains only for the yolofs mount itself.

`BIND_MOUNTS` / `bind_mount_pseudofs` move to [exec.rs](../../user/cmd/exec.rs)
as raw `libc` calls. The `mnt/{proc,dev,sys}` target dirs must exist in the
staged view; skip any absent source/target, as `bind_mount_pseudofs` already
does.

Tradeoff: a bare `ls .yolofs/mnt/proc` from *outside* a running command now
shows nothing (the mount only exists inside a live `yolo run`) — acceptable,
since the mount is only meant to be traversed from inside a command.

## Docs

- `docs/cli.md` "Execution Environment", "Privilege Model", "Command execution
  lifecycle", "Capabilities by command": describe the namespace sequence, drop
  `cap_sys_chroot`, explain the `/proc` closure and the userns tradeoff.
- `docs/architecture.md` lifecycle line: "enters a private pid + mount
  namespace, pivots root, runs the command".
- `docs/permissions.md`: "chrooted inside" → "root pivoted onto the mount".

## Testing

- `tests/perm/` **failing-first** test reproducing the confirmed escape: spawn
  an ordinary same-uid process outside the session (holding a secret host file
  outside the session root), then from inside `yolo run` read
  `/proc/<that-host-pid>/root/<secret-path>`. Assert the read **fails** (the pid
  is not visible in the fresh `/proc`, so `ENOENT`). Fails against `chroot`
  (reads the secret), passes after this change. Skip gracefully if the CI
  kernel forbids the setup.
- Existing `yolo run` e2e tests must still pass (same `/`-rooted view, preserved
  cwd, working `/proc` `/dev` `/sys` inside the command).

## Out of scope

- Network / user / uts / ipc namespaces — this plan isolates the filesystem
  root and process view only.
- An init-shim as PID 1 (noted above as a possible follow-on).
- Blocking unprivileged userns creation system-wide.

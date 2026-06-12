# 62 — `RULE_SET`/`RULE_RESOLVE` take an `O_PATH` fd

Replace the path-string payload of the rule ioctls with an `O_PATH` file
descriptor of the target, opened through the mount by the CLI. No backward
compatibility (per AGENTS.md). Rules remain allowed **only on existing
paths** — that restriction is now a documented decision (see
permissions.md), not an implementation accident: inheritance covers
files created later under a ruled ancestor, and `yolofs.toml` holds rules
for paths that don't exist yet until a later mount / `yolo rule` applies
them.

## Motivation

- **One resolution instead of two.** Today the CLI canonicalizes the path,
  then the kernel `kern_path`s the string again. An agent renaming
  directories inside the mount between the two resolutions can redirect
  where a rule lands. With an fd, resolution happens once, at `open()`.
- **Exact in-mount check.** The `-EXDEV` test moves from "whatever the
  string resolved to" to the precise dentry the rule attaches to
  (`f_path.dentry->d_sb == sb`).
- **No `YOLO_PATH_MAX` on rule paths.** Rule targets are currently capped
  at 255 bytes *including* the `<runtime>/mnt` prefix; deep paths fail
  today. Fds have no length limit.
- **Less kernel code.** `yolo_resolve_rule` drops `kern_path` and the
  userspace path copy. Matches the `landlock_add_rule(parent_fd)` precedent
  already cited in permissions.md.

Survey of the other path-crossing surfaces concluded they should **not**
convert: `TRAVEL`/`RESTORE` tree entries (`YOLO_TARGET_PATH`) are
CLI-generated from its own journal, occur many-per-tree (an fd array would
complicate the format), and an fd grants the same power as a path there;
`SNAPSHOT` carries a label, not a path; the mount source string is standard
mount-API practice; `GET_ASK` paths flow kernel→user for display only.

## Wire contract

```c
/* yolofs.h — replaces path_ptr/path_len */
struct yolo_ioc_rule {
    __s32 fd;      /* O_PATH fd of the rule target, opened through the mount */
    __u8  perm;    /* YOLO_PERM_*; UNSET clears (SET); OUT: effective (RESOLVE) */
    __u8  _pad[3];
};
```

8 bytes. Command numbers unchanged: `RULE_SET` `_IOW('A',10)`,
`RULE_RESOLVE` `_IOWR('A',11)`. Symlink-follow semantics move to the CLI's
`open()` (no `O_NOFOLLOW`, matching today's `LOOKUP_FOLLOW`; the final
component is already resolved by `fs::canonicalize` anyway).

## Kernel (kmod/yolofs.h, kmod/ioctl.c)

- `yolo_resolve_rule()`: `copy_from_user` the 8-byte struct, `fget_raw(fd)`
  (reject `-EBADF`), then validate: `f_path.dentry->d_sb` must equal the
  ioctl fd's sb (`-EXDEV`), `d_unlinked()` rejected with `-EINVAL`
  (an fd can reference an unlinked dentry, which `kern_path` never could —
  a rule there pins a dentry no path reaches). Output the dentry with its
  own `path_get` reference; callers keep `path_put` symmetry, the target
  file is `fput` before returning. `fget_raw`, not `fget`/`fdget`: the
  plain variants mask out O_PATH (`FMODE_PATH`) files, and `fdget_raw`'s
  `__fdget_raw` helper is not exported to modules.
- `yolo_rule_set_ioctl` / `yolo_rule_resolve_ioctl`: logic unchanged —
  pin via `dget(dentry)` on first attach exactly as today (the pin must
  outlive the fd). Caller-inside gating unchanged (`RULE_SET` refused from
  inside; `RULE_RESOLVE` allowed).
- `yolo_copy_user_path()` stays (still used by `SNAPSHOT`).

## CLI (user/ioctl.rs, user/config.rs)

- `ioctl.rs`: `YoloIocRule { fd, perm, _pad }`; `set_rule`/`resolve_rule`
  take the target as an open `&File` instead of `&str`. New helper
  `open_rule_target(path) -> Result<File>`: `OpenOptions` +
  `custom_flags(libc::O_PATH | libc::O_CLOEXEC)` (the access mode is
  ignored by the kernel under `O_PATH`).
- `config.rs`: `resolve_through_mount` keeps producing the open path but is
  no longer sent over the wire. `set_rule`/`unset_rule`/`resolve_rule`/
  `apply_rules` open the target and pass the fd. `hide_mountpoint`: the
  tolerated `ENOENT` now surfaces from `open()` rather than the ioctl —
  match on `io::ErrorKind::NotFound` instead of `Errno::ENOENT`.
- `resolve_to_abs` (canonicalize, exists-only) unchanged.

## Docs

- permissions.md: rule set/unset steps, the exists-only decision note, the
  hide-unset resolvability note, Landlock "Rule interface" paragraph and
  comparison-table row. (Updated first, in this change set.)

## Tests

- Unit (`user/ioctl.rs`): struct size becomes 8; drop `make_rule` tests;
  `open_rule_target` on a missing path errors with `NotFound`.
- `tests/cli/`: existing rule tests pass unchanged (behavioral parity).
- `tests/perm/` (new):
  - rule on a path whose through-mount form exceeds 256 bytes — impossible
    before, must work now;
  - set then **unset a `hide` rule** — load-bearing: verifies hidden,
    pinned dentries still resolve for the owner outside the mount.
- `tests/internals/` (new, raw ioctl): fd outside the mount → `EXDEV`;
  fd of an unlinked file → `EINVAL`; closed/bogus fd → `EBADF`.

## Steps

1. Docs (done with this plan).
2. `yolofs.h` + `ioctl.c`.
3. `ioctl.rs` + `config.rs`.
4. Tests above.
5. `make test-vm`.
6. Full AGENTS.md code review (parallel sub-agents), then move this plan to
   `done/`.

## Deviations during implementation

- `ioctl.rs` grew `set_rule_raw(fd, target_fd, perm)` returning the bare
  errno, so the white-box fd-validation tests can pass fds no
  `open_rule_target` would produce.
- `config.rs` got a private `open_target_through_mount` helper folding the
  shared resolve-through-mount + open + context pattern of the four rule
  call sites.
- A regular (non-O_PATH) fd is also accepted as a rule target (`fget_raw`
  takes both); covered by `rule_set_accepts_regular_fd`.
- Unrelated latent bug fixed en route: five tests in
  `tests/cli/test_watch.rs` spawned their reader thread *before* claiming
  daemon status with a blocking `GET_ASK`. If the reader's ask check ran
  first, the kernel insta-denied (no daemon connected, nothing enqueued)
  and `GET_ASK` hung forever. New `claim_daemon` (claim first, O_NONBLOCK)
  and `poll_get_ask` (bounded poll) helpers close the race; reproduced
  live and confirmed via kernel stack dumps before fixing.

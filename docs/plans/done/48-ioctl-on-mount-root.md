# 48 — control ioctls on the mount root (drop the synthetic `.ctl`)

Move all control ioctls (RULE_SET/RESOLVE, GET_ASK/PUT_DECISION, SNAPSHOT,
TRAVEL) off the synthetic `.ctl` file and onto the **mount-root directory**, the
way Btrfs/ext4 expose their ioctls on any dir/file. The handler already derives
the session generically (`YOLO_SB(file_inode(file)->i_sb)`), so it works on a
directory fd unchanged. No ioctl numbers/structs change.

## Why

`.ctl` is a synthetic regular file pinned at the mount root purely to give
userspace an fd to ioctl. Dropping it removes the special inode/dentry +
pinning, and a directory fd is always reachable (`.`/`/`) from inside the
sandbox. (It does *not* remove the inside/outside path difference — the mount is
`<session>/mnt` outside but `/` inside regardless — so `ioctl::open` keeps a
small fallback, just opening a dir instead of `.ctl`.)

## Wrinkle 1 — daemon-claim vs the readdir cursor

`yolo_dir_open` stores the readdir iterator in `file->private_data`
(dir.c:64). The ask daemon currently claims itself with
`file->private_data = (void *)1` (ioctl.c) and `yolo_ctl_release` cleans up when
`private_data` is set. On a **dir** fd those collide. Fix: track the daemon by
**file identity** instead — `sbi->ask_engine.daemon_file` (a `struct file *`).
- `GET_ASK`: `cmpxchg(has_daemon,0,1)`; on success set `daemon_file = file`.
- `yolo_dir_release`: if `file == sbi->ask_engine.daemon_file`, run daemon
  cleanup and clear it (in addition to freeing the readdir iterator).
- Drop all `file->private_data` daemon use.

## Wrinkle 2 — agent vs user (the real authorization)

uid can't tell an agent's command from the user: `yolo exec` drops to the
invoking user's uid, so both run as the same uid. What differs is the **chroot**
— anything in the sandbox (agent commands *and* the interactive `yolo` shell) is
chrooted into the mount; a normal terminal is not. So gating-defeating ops are
refused from inside: in `yolo_ctl_ioctl`, if the caller is chrooted into this
mount (`get_fs_root(current->fs).dentry->d_sb == sb`), reject `RULE_SET`,
`GET_ASK`, `PUT_DECISION` with `-EPERM`. `SNAPSHOT`/`TRAVEL`/`RULE_RESOLVE` are
allowed from inside (they can't escape gating).

We do **not** re-add the old `0600` owner-only check — the mount lives in the
user's own tree, so path perms suffice; control isn't gated by uid.

## Kernel changes

- **dir.c** `yolo_dir_fops`: add `.unlocked_ioctl = yolo_ctl_ioctl` and
  `.compat_ioctl = yolo_ctl_ioctl`; extend `yolo_dir_release` with the
  daemon-cleanup-by-identity check.
- **ioctl.c**: make `yolo_ctl_ioctl` non-static (called from dir.c); add the
  owner check at its entry; rework `yolo_get_ask_ioctl` to claim via
  `daemon_file`; drop `yolo_ctl_open`/`yolo_ctl_release`/`yolo_ctl_fops`.
- **super.c**: drop `yolo_init_ctl`, `ctl_inode`, `ctl_dentry`, and their
  alloc/teardown; set `sbi->owner = current_uid()` in `fill_super`.
- **yolofs.h**: drop `yolo_ctl_fops`, `ctl_inode`, `ctl_dentry`; declare
  `yolo_ctl_ioctl`; add `kuid_t owner` to `yolo_sb_info` and `struct file
  *daemon_file` to the ask engine.

## Userspace changes

- **ioctl.rs** `open()`: open the mount-root **directory** (`<session>/mnt`),
  falling back to `/` on `NotFound` (inside the chroot). Update the module
  comment (control now goes through the mount root, not `.ctl`).
- No other caller changes — all 10 `ioctl::open` callers are unaffected.

## Docs

- Update `.ctl` mentions (permissions.md "daemon connects by opening
  `.yolofs/mnt/.ctl`", staging.md, kmod/userspace comments) to "the mount root".

## Validation

- `make kmod` + `cargo build`.
- `make test-vm` — especially the ask/watch e2e (daemon claim/cleanup reworked)
  and snapshot/travel/rule e2e (ioctl now on the dir). Confirm only one daemon
  at a time still holds (`has_daemon`), and a second `watch` is rejected.
- Re-run the in-shell snapshot PTY check (ioctl reaches the dir from inside).
- Unit: `ioctl::open` opens a dir and falls back to `/`.

## Risks

- `yolo_dir_release` now does daemon cleanup — ensure it fires only for the
  daemon fd (identity compare), not every dir close.
- Opening a directory `O_RDONLY` for ioctl must not perturb readdir state on
  that fd (the daemon fd never iterates; still, keep the iterator alloc intact).
- Owner check must not break the privilege-dropped sandbox (uid == owner there).

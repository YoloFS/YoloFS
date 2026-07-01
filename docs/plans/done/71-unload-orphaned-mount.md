# 71 — Robust `yolo unload` when the project dir was deleted

## Motivation

A YoloFS session has two locations: the durable backing store
`<project>/.yolofs/` (the mount *source*) and the live over-`/` mountpoint under
`/run/user/<uid>/yolofs/<rand>` (the mount *target*). The link between them is
the `.yolofs/mnt` symlink, which records where the target lives.

If the user runs `rm -rf <project>/` **before** unmounting, the `.yolofs/mnt`
symlink dies with the project dir, but the kernel mount — and the module
reference it holds — lives on. Recovery is then impossible:

- `unmount_at` resolves the mountpoint via `mnt_dir` = `readlink(.yolofs/mnt)`.
  With the symlink gone, `mnt_dir` returns the (now non-existent) link path, so
  `mnt.exists()` is false and **the umount is silently skipped**.
- `yolo unload` → `unmount_all` reads `/proc/mounts` but only extracts the
  *source* column, then routes through the same symlink-based `unmount_at`. So
  it also skips the umount. `delete_module` then fails: `module still has 1
  reference(s)`.

The kernel already knows the true mountpoint — it is the second column of the
`/proc/mounts` line. `unmount_all` throws it away.

## Design

`yolo unload` unmounts the mountpoint the **kernel** reports, not the one the
workspace symlink records — the authoritative, location-independent source of
what is actually mounted.

- **`parse_mounts` returns `(source, mountpoint)` pairs.** It already has both
  columns; stop discarding the mountpoint. Match on the fstype field (`== "yolofs"`)
  instead of a ` yolofs ` substring — more precise, same substring-rejection
  behavior.
- **`find_yolo_dirs` → `find_yolo_mounts`.** Returns the `(source, mountpoint)`
  pairs.
- **`unmount_all` unmounts by kernel-reported mountpoint.** For each pair, call
  the shared `umount_or_prompt` (busy-handling included) on the mountpoint
  directly — robust whether or not `.yolofs/` still exists. Then best-effort
  drop the `.yolofs/cwd` symlink if the project dir is still present.
- **Sweep stale runtime mountpoints.** After unmounting, `rmdir` empty leftover
  dirs under `/run/user/<uid>/yolofs/`. A session whose project dir was deleted
  leaves its mountpoint dir behind (the symlink that pointed at it is gone), so
  these accumulate. `rmdir` fails on a non-empty dir (an active mountpoint shows
  the `/` view), which is exactly the safety we want — only truly stale empties
  are removed.

`yolo unmount`/`remount` are unchanged: they operate on a named live session via
`session_dir()`, which legitimately requires `.yolofs/` to exist. `unload` is
the recovery path when the whole project is gone.

## Exposed interfaces

- `mount::umount_or_prompt` → `pub(crate)` (reused by `unmount_all`).
- `utils::runtime_base` → `pub(crate)` (used by the sweep).

## Tests

- Unit: `parse_mounts` returns `(source, mountpoint)` tuples; substring/no-entry
  cases still reject. (Existing `parse_mounts` unit tests updated to the new
  return type.)
- E2E (`tests/cli/test_unmount.rs`): mount a throwaway project, `rm -rf` it,
  then `yolo unload` must succeed, leave no yolofs entry in `/proc/mounts`, and
  sweep the stale mountpoint dir. Drives the global module, so it restores the
  module to loaded before asserting.

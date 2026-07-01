// yolo CLI — exec.rs
//
// Isolate a command in its own pid + mount namespace, pivot_root onto the
// session mountpoint, and exec it, preserving the caller's working directory.
// This backs `yolo run -- <cmd>`. When
// config.auto_snapshot=true, a snapshot is created after the command finishes,
// capturing what the command did — this is how each command (e.g. one agent
// tool-call) becomes a per-command checkpoint.

use crate::config;
use crate::ioctl;
use crate::report;
use anyhow::{Context, Result, bail};
use std::env;
use std::ffi::CString;
use std::io;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process;

/// Paths the child's isolation hook needs, all built **before** the fork so the
/// post-fork hook (which must be async-signal-safe) allocates nothing.
struct Isolation {
    /// `.yolofs/mnt` — the new root we pivot onto.
    mnt: CString,
    /// The caller's original cwd, restored after the pivot.
    cwd: CString,
    /// Mount targets under `mnt` for the pseudo-filesystems.
    proc_target: CString,
    dev_target: CString,
    sys_target: CString,
}

impl Isolation {
    fn new(mnt: &Path, cwd: &Path) -> Result<Self, io::Error> {
        let cstr = |p: &Path| {
            CString::new(p.as_os_str().as_encoded_bytes())
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))
        };
        Ok(Self {
            mnt: cstr(mnt)?,
            cwd: cstr(cwd)?,
            proc_target: cstr(&mnt.join("proc"))?,
            dev_target: cstr(&mnt.join("dev"))?,
            sys_target: cstr(&mnt.join("sys"))?,
        })
    }
}

/// Pre-exec hook: isolate the command in a private mount namespace, give it a
/// fresh `/proc` (plus `/dev` `/sys`), then `pivot_root` onto the yolofs mount
/// and detach the old host root. Runs after fork but before exec, so it only
/// affects the child — which is already PID 1 of a new pid namespace because the
/// parent called `unshare(CLONE_NEWPID)` before spawning (see `run`).
///
/// The fresh `/proc` is what closes the `/proc/<pid>/root` bypass: in the new
/// pid namespace no outside process is visible, and with the old root detached
/// no host mount is reachable. All steps are raw syscalls on pre-built
/// `CString`s — no allocation, no panic.
///
/// No uid/gid drop is needed: the CLI carries `cap_sys_admin` as a file
/// capability (used here for `unshare`/`mount`/`pivot_root`) and already runs as
/// the invoking user. `execve` then clears it for the spawned command — a
/// non-setuid image with no file caps, run by a non-root euid, receives an empty
/// capability set — so the command itself can neither mount nor pivot_root.
unsafe fn isolate_pre_exec(iso: &Isolation) -> Result<(), io::Error> {
    // mount(2) with a null data arg; MS_* flags are c_ulong. Returns the last
    // OS error on failure so the spawn aborts rather than running un-isolated.
    let mount = |src: *const libc::c_char,
                 target: *const libc::c_char,
                 fstype: *const libc::c_char,
                 flags: libc::c_ulong|
     -> Result<(), io::Error> {
        // SAFETY: all pointers are either null or from `iso`'s pre-built,
        // still-live CStrings / static c-string literals.
        if unsafe { libc::mount(src, target, fstype, flags, std::ptr::null()) } != 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    };
    // Most syscalls here return c_int (0 = ok); pivot_root via libc::syscall
    // returns c_long but is only ever 0/-1, so it casts cleanly to c_int.
    let check = |ret: libc::c_int| -> Result<(), io::Error> {
        if ret != 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    };

    unsafe {
        // 1. Private mount namespace for this command.
        check(libc::unshare(libc::CLONE_NEWNS))?;
        // 2. Make root-tree propagation private, else pivot_root returns EINVAL
        //    and the old-root detach below could propagate to the host ns.
        mount(
            std::ptr::null(),
            c"/".as_ptr(),
            std::ptr::null(),
            libc::MS_REC | libc::MS_PRIVATE,
        )?;
        // 3. Fresh procfs — in the new pid namespace it shows only this
        //    command's own processes, so no `/proc/<pid>/root` reaches outside.
        mount(
            c"proc".as_ptr(),
            iso.proc_target.as_ptr(),
            c"proc".as_ptr(),
            0,
        )?;
        // 4. Bind /dev and /sys (recursively, so /dev carries its devpts/shm
        //    submounts); they expose no per-process root symlinks.
        mount(
            c"/dev".as_ptr(),
            iso.dev_target.as_ptr(),
            std::ptr::null(),
            libc::MS_BIND | libc::MS_REC,
        )?;
        mount(
            c"/sys".as_ptr(),
            iso.sys_target.as_ptr(),
            std::ptr::null(),
            libc::MS_BIND | libc::MS_REC,
        )?;
        // 5. pivot_root onto the mount (runc idiom: chdir there, pivot "." onto
        //    ".", so no put_old dir is needed; .yolofs/mnt is a real mountpoint).
        check(libc::chdir(iso.mnt.as_ptr()))?;
        check(libc::syscall(libc::SYS_pivot_root, c".".as_ptr(), c".".as_ptr()) as libc::c_int)?;
        // 6. Detach the old host root, now stacked on "." — unreachable after.
        check(libc::umount2(c".".as_ptr(), libc::MNT_DETACH))?;
        // 7. Restore the caller's working directory (now resolved through the
        //    mount, same absolute path).
        check(libc::chdir(iso.cwd.as_ptr()))?;
    }
    Ok(())
}

/// Outcome of the post-command auto-snapshot, so callers can decide how to
/// surface it (quiet `yolo run --no-review -- <cmd>` prints a terse line; the default
/// `yolo run -- <cmd>` folds the id into its review summary).
pub enum Snapshot {
    /// A snapshot was created with this gen id.
    Created(u64),
    /// The command staged nothing, so no snapshot was made.
    NoChanges,
    /// Auto-snapshot is disabled in config.
    Off,
}

/// Print the post-command snapshot outcome for the quiet `yolo run --no-review -- <cmd>`
/// (to stderr). The snapshot's name is omitted — it just echoes the command you
/// already typed; `timeline`/`journal` still show it.
pub fn announce(snapshot: &Snapshot) {
    match snapshot {
        Snapshot::Created(gen_id) => {
            report::info(format!("snapshot {gen_id}"));
        }
        Snapshot::NoChanges => {
            report::hint("no changes, skipping snapshot");
        }
        Snapshot::Off => {}
    }
}

/// Spawn a command under yolofs and wait for it to exit. Returns the process
/// exit code (0 = success) and the post-command auto-snapshot outcome.
pub fn run(exec_args: &[String]) -> Result<(u8, Snapshot)> {
    let yolo_dir = super::mount::ensure_mounted()?;
    let mnt = crate::utils::mnt_dir(&yolo_dir);
    let cwd = env::current_dir().context("getting cwd")?;

    let Some((cmd, args)) = exec_args.split_first() else {
        bail!("no command given — usage: `yolo run -- <cmd>`");
    };

    let iso = Isolation::new(&mnt, &cwd).context("preparing command isolation")?;

    let mut command = process::Command::new(cmd);
    command
        .args(args)
        .env("YOLO_SESSION", yolo_dir.to_string_lossy().as_ref());

    // Put the command in a fresh pid namespace. `unshare(CLONE_NEWPID)` places
    // the *next* forked child (the command) as PID 1 of a new pid namespace; it
    // can't be done from the pre_exec hook, which runs post-fork and execs
    // without forking again. The child then creates its own mount namespace and
    // mounts a fresh /proc that sees only this namespace. `yolo` forks exactly
    // once more (this command) and then only issues ioctls before exiting, so
    // unsharing its child-pidns here has no wider effect.
    if unsafe { libc::unshare(libc::CLONE_NEWPID) } != 0 {
        return Err(io::Error::last_os_error()).context("unshare(CLONE_NEWPID)");
    }

    let status = unsafe {
        command
            .pre_exec(move || isolate_pre_exec(&iso))
            .status()
            .with_context(|| format!("spawning {cmd}"))?
    };

    // The command's exit code is propagated as ours and its output already
    // told the story — no "command exited with N" announcement.
    let code = status.code().unwrap_or(1) as u8;

    // Snapshot after the command so it captures what the command did (skipped
    // when nothing was staged, to avoid empty snapshots). The name — a
    // display-only label shown in `timeline`/`journal` — is just the command;
    // how the outcome is surfaced is left to the caller.
    let snapshot = if config::load_config().auto_snapshot {
        let cmd_desc = exec_args.join(" ");
        match auto_snapshot(&format!("after {cmd_desc}")) {
            Ok(Some(gen_id)) => Snapshot::Created(gen_id),
            Ok(None) => Snapshot::NoChanges,
            Err(e) => {
                report::warn(format!("snapshot failed: {e:#}"));
                Snapshot::NoChanges
            }
        }
    } else {
        Snapshot::Off
    };

    Ok((code, snapshot))
}

/// Create a snapshot only if there are staged changes (kernel-side check).
/// Returns the new snapshot's gen id, or `None` if nothing changed.
fn auto_snapshot(name: &str) -> Result<Option<u64>> {
    let yolofs = crate::utils::session_dir()?;
    let ctl_file = ioctl::open(&yolofs).context("opening ctl for snapshot")?;
    let gen_id = ioctl::snapshot(&ctl_file, name, ioctl::YOLO_SNAPSHOT_IF_CHANGED)?;
    Ok((gen_id != 0).then_some(gen_id))
}

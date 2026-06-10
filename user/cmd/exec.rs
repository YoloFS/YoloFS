// yolo CLI — exec.rs
//
// Chroot into the session mountpoint and exec a command, preserving the
// caller's working directory. This backs `yolo run -- <cmd>`. When
// config.auto_snapshot=true, a snapshot is created after the command finishes,
// capturing what the command did — this is how each command (e.g. one agent
// tool-call) becomes a per-command checkpoint.

use crate::config;
use crate::ioctl;
use anyhow::{Context, Result, bail};
use colored::Colorize;
use std::env;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process;

/// Pre-exec hook: chroot into mnt, then chdir back to the original cwd.
/// Called after fork but before exec, so it only affects the child.
///
/// No uid/gid drop is needed. The CLI carries `cap_sys_chroot` (used here) and
/// `cap_sys_admin` as file capabilities rather than setuid root, so it already
/// runs as the invoking user. `execve` then clears both capabilities for the
/// spawned command — a non-setuid image with no file caps, run by a non-root
/// euid, receives an empty capability set — so the command itself can neither
/// chroot nor mount.
unsafe fn chroot_pre_exec(mnt: &Path, cwd: &Path) -> Result<(), std::io::Error> {
    let mnt_cstr = std::ffi::CString::new(mnt.as_os_str().as_encoded_bytes())
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e))?;
    let cwd_cstr = std::ffi::CString::new(cwd.as_os_str().as_encoded_bytes())
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e))?;

    unsafe {
        if libc::chroot(mnt_cstr.as_ptr()) != 0 {
            return Err(std::io::Error::last_os_error());
        }
        if libc::chdir(cwd_cstr.as_ptr()) != 0 {
            return Err(std::io::Error::last_os_error());
        }
    }
    Ok(())
}

/// Outcome of the post-command auto-snapshot, so callers can decide how to
/// surface it (quiet `yolo run -q -- <cmd>` prints a terse line; the default
/// `yolo run -- <cmd>` folds the id into its review summary).
pub enum Snapshot {
    /// A snapshot was created with this gen id.
    Created(u64),
    /// The command staged nothing, so no snapshot was made.
    NoChanges,
    /// Auto-snapshot is disabled in config.
    Off,
}

/// Print the post-command snapshot outcome for the quiet `yolo run -q -- <cmd>`
/// (to stderr). The snapshot's name is omitted — it just echoes the command you
/// already typed; `timeline`/`journal` still show it.
pub fn announce(snapshot: &Snapshot) {
    match snapshot {
        Snapshot::Created(gen_id) => {
            eprintln!("{}", format!("snapshot {gen_id}").cyan().bold());
        }
        Snapshot::NoChanges => {
            eprintln!("{}", "yolo: no changes, skipping snapshot".dimmed());
        }
        Snapshot::Off => {}
    }
}

/// Spawn a command under yolofs and wait for it to exit. Returns the process
/// exit code (0 = success) and the post-command auto-snapshot outcome.
pub fn run(exec_args: &[String]) -> Result<(u8, Snapshot)> {
    let yolo_dir = crate::utils::session_dir()?;
    let mnt = crate::utils::mnt_dir(&yolo_dir);
    let cwd = env::current_dir().context("getting cwd")?;

    if !mnt.exists() {
        bail!("mount point does not exist — run `yolo mount` first");
    }

    let Some((cmd, args)) = exec_args.split_first() else {
        bail!("no command given — usage: `yolo run -- <cmd>`");
    };

    let mut command = process::Command::new(cmd);
    command
        .args(args)
        .env("YOLO_SESSION", yolo_dir.to_string_lossy().as_ref());

    let status = unsafe {
        command
            .pre_exec(move || chroot_pre_exec(&mnt, &cwd))
            .status()
            .with_context(|| format!("spawning {cmd}"))?
    };

    let code = status.code().unwrap_or(1) as u8;
    if code != 0 {
        eprintln!("{} {}", "yolo: command exited with".red(), code);
    }

    // Snapshot after the command so it captures what the command did (skipped
    // when nothing was staged, to avoid empty snapshots). The name — still
    // stored for `timeline`/`journal`/travel-by-name — is just the command; how
    // the outcome is surfaced is left to the caller.
    let snapshot = if config::load_config().auto_snapshot {
        let cmd_desc = exec_args.join(" ");
        match auto_snapshot(&format!("after {cmd_desc}")) {
            Ok(Some(gen_id)) => Snapshot::Created(gen_id),
            Ok(None) => Snapshot::NoChanges,
            Err(e) => {
                eprintln!("{} {:#}", "yolo: snapshot failed:".yellow(), e);
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

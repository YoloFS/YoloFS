// yolo CLI — exec.rs
//
// `yolo exec [-- cmd]` — chroot into .yolofs/mnt and exec a command,
// preserving the caller's working directory.
// When config.snapshot=true, a snapshot is created after the command
// finishes, capturing what the command did.

use crate::config;
use crate::ioctl;
use anyhow::{Context, Result, bail};
use colored::Colorize;
use std::env;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process;

/// Pre-exec hook: chroot into mnt, chdir back to the original cwd, then
/// permanently drop root privileges to the invoking user's real uid/gid.
/// Called after fork but before exec, so it only affects the child.
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

        // Drop root: restore the invoking user's real gid/uid.
        // Order matters — setgid first, because setuid drops the ability to
        // change gid. Both calls are permanent (no re-elevation possible).
        let real_gid = libc::getgid();
        let real_uid = libc::getuid();
        if libc::setgid(real_gid) != 0 {
            return Err(std::io::Error::last_os_error());
        }
        if libc::setuid(real_uid) != 0 {
            return Err(std::io::Error::last_os_error());
        }
    }
    Ok(())
}

/// Spawn a command in the sandbox and wait for it to exit.
/// Returns the process exit code (0 = success).
pub fn run(exec_args: &[String]) -> Result<u8> {
    let yolo_dir = crate::utils::session_dir()?;
    let mnt = yolo_dir.join("mnt");
    let cwd = env::current_dir().context("getting cwd")?;

    if !mnt.exists() {
        bail!("mount point .yolofs/mnt/ does not exist — run `yolo mount` first");
    }

    let default_shell = "sh".to_string();

    let (cmd, args) = if exec_args.is_empty() {
        eprintln!("{}", "yolo: entering sandbox (exit to return)".cyan());
        (default_shell.clone(), vec![])
    } else {
        (exec_args[0].clone(), exec_args[1..].to_vec())
    };

    let status = unsafe {
        process::Command::new(&cmd)
            .args(&args)
            .env("YOLO_SESSION", yolo_dir.to_string_lossy().as_ref())
            .pre_exec(move || chroot_pre_exec(&mnt, &cwd))
            .status()
            .with_context(|| format!("spawning {cmd}"))?
    };

    let code = status.code().unwrap_or(1) as u8;
    if code != 0 {
        eprintln!("{} {}", "yolo: command exited with".red(), code);
    }

    // Snapshot after the command so the snapshot captures what the command did.
    // Skip if the command produced no staged changes to avoid empty snapshots.
    if config::load_config().snapshot {
        let cmd_desc = if exec_args.is_empty() {
            default_shell.clone()
        } else {
            exec_args.join(" ")
        };
        let chk_name = format!("after {cmd_desc}");

        match auto_snapshot(&chk_name) {
            Ok(true) => {} // snapshot created (message printed by snapshot::create path)
            Ok(false) => {
                eprintln!("{}", "yolo: no changes, skipping snapshot".dimmed());
            }
            Err(e) => {
                eprintln!("{} {:#}", "yolo: snapshot failed:".yellow(), e);
            }
        }
    }

    Ok(code)
}

/// Create a snapshot only if there are staged changes (kernel-side check).
fn auto_snapshot(name: &str) -> Result<bool> {
    let yolofs = crate::utils::session_dir()?;
    let ctl_file = ioctl::open(&yolofs).context("opening ctl for mark")?;
    let gen_id = ioctl::mark(&ctl_file, name, ioctl::YOLO_MARK_IF_CHANGED)?;
    if gen_id == 0 {
        return Ok(false);
    }
    eprintln!(
        "{} {}",
        format!("snapshot [{gen_id}]").cyan().bold(),
        name.dimmed()
    );
    Ok(true)
}

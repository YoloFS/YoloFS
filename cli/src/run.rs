// agfs CLI — run.rs
//
// `agfs run [-- cmd]` — chroot into .agfs/mnt and run a command,
// preserving the caller's working directory.

use anyhow::{bail, Context, Result};
use colored::Colorize;
use std::env;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process;

/// Pre-exec hook: chroot into mnt, then chdir back to the original cwd.
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
    }
    Ok(())
}

/// Spawn a command in the sandbox and wait for it to exit.
/// Returns the process exit code (0 = success).
pub fn run(exec_args: &[String]) -> Result<u8> {
    let agfs_dir = crate::session_dir()?;
    let mnt = agfs_dir.join("mnt");
    let cwd = env::current_dir().context("getting cwd")?;

    if !mnt.exists() {
        bail!("mount point .agfs/mnt/ does not exist — run `agfs mount` first");
    }

    let (cmd, args) = if exec_args.is_empty() {
        let shell = env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string());
        eprintln!("{}", "agfs: entering sandbox (exit to return)".cyan());
        (shell, vec![])
    } else {
        (exec_args[0].clone(), exec_args[1..].to_vec())
    };

    let mnt2 = mnt.clone();
    let cwd2 = cwd.clone();
    let status = unsafe {
        process::Command::new(&cmd)
            .args(&args)
            .env("AGFS_SESSION", agfs_dir.to_string_lossy().as_ref())
            .pre_exec(move || chroot_pre_exec(&mnt2, &cwd2))
            .status()
            .with_context(|| format!("spawning {cmd}"))?
    };

    let code = status.code().unwrap_or(1) as u8;
    if code != 0 {
        eprintln!("{} {}", "agfs: command exited with".red(), code);
    }

    Ok(code)
}

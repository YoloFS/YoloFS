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

/// bash `PROMPT_COMMAND` bootstrap for the interactive shell. A DEBUG trap
/// records each command (`$BASH_COMMAND`, captured at run time — reliable,
/// unlike history/`fc`), and a precmd *function* snapshots it after it finishes
/// (only if something changed), printing `snapshot [N] <cmd>` — the same
/// per-command checkpoint trail an agent gets. We deliberately do NOT run
/// `yolo status` here: its added/modified/deleted classification needs the
/// lower filesystem, which the chroot hides behind the overlay, so it
/// misreports inside the mount. Review state with `yolo status`/`diff` from
/// outside. Because bash doesn't fire the DEBUG trap inside functions (no
/// `functrace`), the trap never captures the hook's own commands. `$?` is
/// saved and restored so the hook doesn't clobber the command's exit status.
/// This runs once on the first prompt, then replaces itself with the function.
const PER_COMMAND_HOOK: &str = "\
__yolo_precmd() { local rc=$?; [ -n \"$__yl\" ] && yolo snapshot --if-changed \"$__yl\"; __yl=; return $rc; }; \
trap '[ \"$BASH_COMMAND\" = __yolo_precmd ] || __yl=$BASH_COMMAND' DEBUG; \
PROMPT_COMMAND=__yolo_precmd";

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

/// Spawn a command under yolofs and wait for it to exit.
/// Returns the process exit code (0 = success).
pub fn run(exec_args: &[String]) -> Result<u8> {
    let yolo_dir = crate::utils::session_dir()?;
    let mnt = yolo_dir.join("mnt");
    let cwd = env::current_dir().context("getting cwd")?;

    if !mnt.exists() {
        bail!("mount point .yolofs/mnt/ does not exist — run `yolo mount` first");
    }

    // Interactive shell → bash, so the per-command hook below can run (sh has
    // no PROMPT_COMMAND). A one-off command runs exactly as given.
    let interactive = exec_args.is_empty();
    let (cmd, args) = if interactive {
        eprintln!("{}", "yolo: entering yolofs (exit to return)".cyan());
        ("bash".to_string(), vec![])
    } else {
        (exec_args[0].clone(), exec_args[1..].to_vec())
    };

    let mut command = process::Command::new(&cmd);
    command
        .args(&args)
        .env("YOLO_SESSION", yolo_dir.to_string_lossy().as_ref());
    if interactive {
        command.env("PROMPT_COMMAND", PER_COMMAND_HOOK);
    }

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

    // Snapshot after the command so the snapshot captures what the command did.
    // Skip if the command produced no staged changes to avoid empty snapshots.
    if config::load_config().snapshot {
        let cmd_desc = if interactive {
            cmd.clone()
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
    let ctl_file = ioctl::open(&yolofs).context("opening ctl for snapshot")?;
    let gen_id = ioctl::snapshot(&ctl_file, name, ioctl::YOLO_SNAPSHOT_IF_CHANGED)?;
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

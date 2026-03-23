// agfs CLI — exec.rs
//
// `agfs exec [-- cmd]` — join the mount daemon's namespace, pivot_root into
// .agfs/mnt, and exec a command, preserving the caller's working directory.
// When config.checkpoint=true, a checkpoint is created after the command
// finishes, capturing what the command did.

use crate::config;
use crate::ioctl;
use anyhow::{bail, Context, Result};
use colored::Colorize;
use std::env;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process;

/// Drop all capabilities so user commands run without elevated privileges.
/// This prevents CAP_DAC_OVERRIDE from bypassing directory permission checks
/// on the base filesystem, and generally follows the principle of least privilege.
unsafe fn drop_caps() {
    // PR_SET_NO_NEW_PRIVS: prevent regaining caps via setuid binaries
    unsafe {
        libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0);
    }

    // Drop all capabilities from all sets.
    // capset with empty data clears everything.
    #[repr(C)]
    struct CapHeader {
        version: u32,
        pid: i32,
    }
    #[repr(C)]
    #[derive(Copy, Clone)]
    struct CapData {
        effective: u32,
        permitted: u32,
        inheritable: u32,
    }
    let header = CapHeader {
        version: 0x20080522, // _LINUX_CAPABILITY_VERSION_3
        pid: 0,
    };
    let data = [CapData {
        effective: 0,
        permitted: 0,
        inheritable: 0,
    }; 2];
    unsafe {
        libc::syscall(libc::SYS_capset, &header as *const CapHeader, data.as_ptr());
    }
}

/// Pre-exec hook: join the daemon's namespace, pivot_root into mnt, then
/// exec the command. Called after fork but before exec.
///
/// Steps:
/// 1. setns into daemon's user namespace (same uid → CAP_SYS_ADMIN in target)
/// 2. setns into daemon's mount namespace (see agfs mount)
/// 3. unshare(CLONE_NEWNS) — private child mount namespace for pivot_root
/// 4. pivot_root(".", ".") — agfs mount becomes new root
/// 5. umount old root via saved fd
/// 6. chdir to original working directory
unsafe fn namespace_pre_exec(
    daemon_pid: u32,
    mnt: &Path,
    cwd: &Path,
) -> Result<(), std::io::Error> {
    use std::ffi::CString;

    let mnt_cstr = CString::new(mnt.as_os_str().as_encoded_bytes())
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e))?;
    let cwd_cstr = CString::new(cwd.as_os_str().as_encoded_bytes())
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e))?;
    let dot = CString::new(".").unwrap();

    unsafe {
        // 1. setns into daemon's user namespace
        let user_ns = CString::new(format!("/proc/{daemon_pid}/ns/user")).unwrap();
        let user_fd = libc::open(user_ns.as_ptr(), libc::O_RDONLY);
        if user_fd < 0 {
            return Err(std::io::Error::last_os_error());
        }
        if libc::setns(user_fd, libc::CLONE_NEWUSER) != 0 {
            libc::close(user_fd);
            return Err(std::io::Error::last_os_error());
        }
        libc::close(user_fd);

        // 2. setns into daemon's PID namespace (for /proc access)
        let pid_ns = CString::new(format!("/proc/{daemon_pid}/ns/pid")).unwrap();
        let pid_fd = libc::open(pid_ns.as_ptr(), libc::O_RDONLY);
        if pid_fd >= 0 {
            // Non-fatal: if PID ns join fails, /proc just won't work
            libc::setns(pid_fd, libc::CLONE_NEWPID);
            libc::close(pid_fd);
        }

        // 3. setns into daemon's mount namespace
        let mnt_ns = CString::new(format!("/proc/{daemon_pid}/ns/mnt")).unwrap();
        let mnt_fd = libc::open(mnt_ns.as_ptr(), libc::O_RDONLY);
        if mnt_fd < 0 {
            return Err(std::io::Error::last_os_error());
        }
        if libc::setns(mnt_fd, libc::CLONE_NEWNS) != 0 {
            libc::close(mnt_fd);
            return Err(std::io::Error::last_os_error());
        }
        libc::close(mnt_fd);

        // 4. Private child mount namespace so pivot_root doesn't affect
        //    the daemon or other exec sessions
        if libc::unshare(libc::CLONE_NEWNS) != 0 {
            return Err(std::io::Error::last_os_error());
        }

        // 4. Save fd to old root, then pivot_root(".", ".")
        let old_root_fd = libc::open(b"/\0".as_ptr() as _, libc::O_RDONLY | libc::O_DIRECTORY);
        if old_root_fd < 0 {
            return Err(std::io::Error::last_os_error());
        }
        if libc::chdir(mnt_cstr.as_ptr()) != 0 {
            libc::close(old_root_fd);
            return Err(std::io::Error::last_os_error());
        }
        if libc::syscall(libc::SYS_pivot_root, dot.as_ptr(), dot.as_ptr()) != 0 {
            libc::close(old_root_fd);
            return Err(std::io::Error::last_os_error());
        }

        // 6. Detach old root
        libc::fchdir(old_root_fd);
        libc::umount2(dot.as_ptr(), libc::MNT_DETACH);
        libc::close(old_root_fd);

        // 7. Mount fresh /proc (we're in the daemon's PID namespace)
        let proc_cstr = CString::new("/proc").unwrap();
        libc::mount(
            proc_cstr.as_ptr(),
            proc_cstr.as_ptr(),
            proc_cstr.as_ptr(),
            0,
            std::ptr::null(),
        );
        // Non-fatal if this fails — /proc just won't be available

        // 8. chdir to original working directory
        if libc::chdir(cwd_cstr.as_ptr()) != 0 {
            return Err(std::io::Error::last_os_error());
        }

        // 9. Drop all capabilities — user commands should not have
        // CAP_DAC_OVERRIDE or any other elevated privileges.
        drop_caps();
    }
    Ok(())
}

/// Pre-exec hook for when we're already in the namespace: just pivot_root
/// and mount /proc (no setns needed).
unsafe fn pivot_only_pre_exec(mnt: &Path, cwd: &Path) -> Result<(), std::io::Error> {
    use std::ffi::CString;

    let mnt_cstr = CString::new(mnt.as_os_str().as_encoded_bytes())
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e))?;
    let cwd_cstr = CString::new(cwd.as_os_str().as_encoded_bytes())
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e))?;
    let dot = CString::new(".").unwrap();

    unsafe {
        // Private mount namespace so pivot_root doesn't affect others
        if libc::unshare(libc::CLONE_NEWNS) != 0 {
            let e = std::io::Error::last_os_error();
            eprintln!("pivot_only: unshare(NEWNS) failed: {e}");
            return Err(e);
        }

        let old_root_fd = libc::open(b"/\0".as_ptr() as _, libc::O_RDONLY | libc::O_DIRECTORY);
        if old_root_fd < 0 {
            let e = std::io::Error::last_os_error();
            eprintln!("pivot_only: open(/) failed: {e}");
            return Err(e);
        }
        if libc::chdir(mnt_cstr.as_ptr()) != 0 {
            let e = std::io::Error::last_os_error();
            eprintln!("pivot_only: chdir(mnt) failed: {e}");
            libc::close(old_root_fd);
            return Err(e);
        }
        if libc::syscall(libc::SYS_pivot_root, dot.as_ptr(), dot.as_ptr()) != 0 {
            let e = std::io::Error::last_os_error();
            eprintln!("pivot_only: pivot_root failed: {e}");
            libc::close(old_root_fd);
            return Err(e);
        }

        libc::fchdir(old_root_fd);
        libc::umount2(dot.as_ptr(), libc::MNT_DETACH);
        libc::close(old_root_fd);

        // Mount /proc
        let proc_cstr = CString::new("/proc").unwrap();
        libc::mount(
            proc_cstr.as_ptr(),
            proc_cstr.as_ptr(),
            proc_cstr.as_ptr(),
            0,
            std::ptr::null(),
        );

        if libc::chdir(cwd_cstr.as_ptr()) != 0 {
            let e = std::io::Error::last_os_error();
            eprintln!("pivot_only: chdir(cwd) failed: {e}");
            return Err(e);
        }

        drop_caps();
    }
    Ok(())
}

/// Read the daemon pid from .agfs/pid.
fn read_daemon_pid(agfs_dir: &Path) -> Result<u32> {
    let pid_path = agfs_dir.join("pid");
    let content = std::fs::read_to_string(&pid_path)
        .with_context(|| format!("reading {} — is `agfs mount` running?", pid_path.display()))?;
    content
        .trim()
        .parse::<u32>()
        .with_context(|| format!("parsing pid from {}", pid_path.display()))
}

/// Spawn a command in the sandbox and wait for it to exit.
/// Returns the process exit code (0 = success).
pub fn run(exec_args: &[String]) -> Result<u8> {
    let agfs_dir = crate::utils::session_dir()?;
    let mnt = agfs_dir.join("mnt");
    let cwd = env::current_dir().context("getting cwd")?;

    if !mnt.exists() {
        bail!("mount point .agfs/mnt/ does not exist — run `agfs mount` first");
    }

    // Detect if already in the daemon's namespace (e.g. called from
    // run_in_namespace in tests). If so, skip setns + pivot_root.
    let already_in_ns = {
        use std::os::unix::fs::MetadataExt;
        matches!(
            (std::fs::metadata(&mnt), std::fs::metadata(&agfs_dir)),
            (Ok(m), Ok(p)) if m.dev() != p.dev()
        )
    };

    if !already_in_ns {
        let daemon_pid = read_daemon_pid(&agfs_dir)?;
        let proc_dir = format!("/proc/{daemon_pid}");
        if !Path::new(&proc_dir).exists() {
            bail!("mount daemon (pid {daemon_pid}) is not running — run `agfs mount` first");
        }
    }

    let default_shell = env::var("SHELL").unwrap_or_else(|_| "sh".to_string());

    let (cmd, args) = if exec_args.is_empty() {
        eprintln!("{}", "agfs: entering sandbox (exit to return)".cyan());
        (default_shell.clone(), vec![])
    } else {
        (exec_args[0].clone(), exec_args[1..].to_vec())
    };

    let status = if already_in_ns {
        // Already in namespace — skip setns but still pivot_root so
        // paths resolve through the agfs mount.
        unsafe {
            process::Command::new(&cmd)
                .args(&args)
                .env("AGFS_SESSION", agfs_dir.to_string_lossy().as_ref())
                .pre_exec(move || pivot_only_pre_exec(&mnt, &cwd))
                .status()
                .with_context(|| format!("spawning {cmd}"))?
        }
    } else {
        let daemon_pid = read_daemon_pid(&agfs_dir)?;
        unsafe {
            process::Command::new(&cmd)
                .args(&args)
                .env("AGFS_SESSION", agfs_dir.to_string_lossy().as_ref())
                .pre_exec(move || namespace_pre_exec(daemon_pid, &mnt, &cwd))
                .status()
                .with_context(|| format!("spawning {cmd}"))?
        }
    };

    let code = status.code().unwrap_or(1) as u8;
    if code != 0 {
        eprintln!("{} {}", "agfs: command exited with".red(), code);
    }

    // Checkpoint after the command so the checkpoint captures what the command did.
    // Skip if the command produced no staged changes to avoid empty checkpoints.
    if config::load_config().checkpoint {
        let cmd_desc = if exec_args.is_empty() {
            default_shell.clone()
        } else {
            exec_args.join(" ")
        };
        let chk_name = format!("after {cmd_desc}");

        match auto_checkpoint(&chk_name) {
            Ok(true) => {} // checkpoint created (message printed by checkpoint::create path)
            Ok(false) => {
                eprintln!("{}", "agfs: no changes, skipping checkpoint".dimmed());
            }
            Err(e) => {
                eprintln!("{} {:#}", "agfs: checkpoint failed:".yellow(), e);
            }
        }
    }

    Ok(code)
}

/// Create a checkpoint only if there are staged changes (kernel-side check).
/// Runs in a forked child to avoid polluting the parent's namespace state
/// (join_daemon_namespace does setns which is irreversible).
fn auto_checkpoint(name: &str) -> Result<bool> {
    let agfs = crate::utils::session_dir()?;

    // Use a pipe to communicate the result back from the child.
    let mut pipe_fds = [0i32; 2];
    if unsafe { libc::pipe(pipe_fds.as_mut_ptr()) } != 0 {
        anyhow::bail!("pipe failed: {}", std::io::Error::last_os_error());
    }

    match unsafe { libc::fork() } {
        -1 => anyhow::bail!("fork failed: {}", std::io::Error::last_os_error()),
        0 => {
            // Child: do the checkpoint in the namespace.
            unsafe { libc::close(pipe_fds[0]) };
            let result: u8 = (|| -> Result<u8> {
                crate::utils::join_daemon_namespace(&agfs)?;
                let ctl_file = ioctl::open(&agfs).context("opening ctl for checkpoint")?;
                let gen_id = ioctl::create_checkpoint(&ctl_file, name, ioctl::AGFS_CHK_IF_CHANGED)?;
                if gen_id == 0 {
                    return Ok(0); // no changes
                }
                eprintln!(
                    "{} {}",
                    format!("checkpoint [{gen_id}]").cyan().bold(),
                    name.dimmed()
                );
                Ok(1) // checkpoint created
            })()
            .unwrap_or_else(|e| {
                eprintln!("{} {:#}", "agfs: checkpoint failed:".yellow(), e);
                2 // error
            });
            unsafe {
                libc::write(pipe_fds[1], &result as *const u8 as _, 1);
                libc::close(pipe_fds[1]);
                libc::_exit(0);
            }
        }
        child_pid => {
            // Parent: read result from child.
            unsafe { libc::close(pipe_fds[1]) };
            let mut result: u8 = 2;
            unsafe {
                libc::read(pipe_fds[0], &mut result as *mut u8 as _, 1);
                libc::close(pipe_fds[0]);
                libc::waitpid(child_pid, std::ptr::null_mut(), 0);
            }
            match result {
                0 => Ok(false),
                1 => Ok(true),
                _ => anyhow::bail!("auto-checkpoint failed"),
            }
        }
    }
}

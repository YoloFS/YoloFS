//! Sandbox directory management and command execution.
use anyhow::{Context, Result};
use nix::libc::{getpgrp, tcsetpgrp};
use nix::mount::{MsFlags, mount};
use nix::sched::{CloneFlags, unshare};
use nix::sys::signal::Signal::{SIGTTIN, SIGTTOU};
use nix::sys::signal::{SigHandler, signal};
use nix::sys::wait::waitpid;
use nix::unistd::{ForkResult, chroot, fork};
use std::env;
use std::fs::{self, File, Permissions};
use std::os::fd::AsRawFd as _;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const APPARMOR_USERNS_SYSCTL: &str = "/proc/sys/kernel/apparmor_restrict_unprivileged_userns";
const CAPTURE_DIR_IN_CHROOT: &str = "/.agfs-capture";

/// Represents a sandbox directory structure.
pub struct Sandbox {
    pub root: PathBuf,
    pub upperdir: PathBuf,
    pub workdir: PathBuf,
    pub temproot: PathBuf,
}

impl Sandbox {
    /// Create a sandbox in an existing directory.
    pub fn new_at(path: PathBuf) -> Result<Self> {
        if !path.exists() {
            fs::create_dir_all(&path).context("Failed to create sandbox directory")?;
        }
        Self::init(path)
    }

    fn init(root: PathBuf) -> Result<Self> {
        let upperdir = root.join("upperdir");
        let workdir = root.join("workdir");
        let temproot = root.join("temproot");

        fs::create_dir_all(&upperdir).context("Failed to create upperdir")?;
        fs::create_dir_all(&workdir).context("Failed to create workdir")?;
        fs::create_dir_all(&temproot).context("Failed to create temproot")?;

        Ok(Self {
            root,
            upperdir,
            workdir,
            temproot,
        })
    }
}

pub struct SandboxRunResult {
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
}

/// Check if unprivileged user namespaces are allowed and try to enable them if not.
fn ensure_userns_allowed() -> Result<()> {
    if !std::path::Path::new(APPARMOR_USERNS_SYSCTL).exists() {
        return Ok(());
    }

    let value = fs::read_to_string(APPARMOR_USERNS_SYSCTL)
        .context("Failed to read apparmor_restrict_unprivileged_userns")?;
    let value = value.trim();

    if value == "0" {
        return Ok(());
    }

    Command::new("sudo")
        .args([
            "sysctl",
            "-w",
            "kernel.apparmor_restrict_unprivileged_userns=0",
        ])
        .status()?;

    Ok(())
}

enum RootEntry {
    Dir(PathBuf),
    Symlink(PathBuf, PathBuf),
}

fn get_root_entries() -> Result<Vec<RootEntry>> {
    let mut entries = Vec::new();
    for entry in fs::read_dir("/")? {
        let entry = entry?;
        let path = entry.path();
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        // Skip special filesystems and tmpfs mounts
        if matches!(name, "boot" | "dev" | "proc" | "sys" | "run" | "") {
            continue;
        }
        if path.is_symlink() {
            entries.push(RootEntry::Symlink(path.clone(), fs::read_link(&path)?));
        } else if path.is_dir() {
            entries.push(RootEntry::Dir(path));
        }
    }
    Ok(entries)
}

fn prepare_sandbox(sandbox: &Sandbox) -> Result<()> {
    let entries = get_root_entries()?;
    for entry in &entries {
        match entry {
            RootEntry::Dir(path) => {
                let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
                let upper = sandbox.upperdir.join(name);
                let work = sandbox.workdir.join(name);
                let merged = sandbox.temproot.join(name);
                fs::create_dir_all(&upper)?;
                fs::create_dir_all(&work)?;
                fs::create_dir_all(&merged)?;
                if let Ok(meta) = path.metadata() {
                    let perm = Permissions::from_mode(meta.permissions().mode());
                    fs::set_permissions(&upper, perm.clone()).expect("Failed to set permissions");
                    fs::set_permissions(&merged, perm).expect("Failed to set permissions");
                }
            }
            RootEntry::Symlink(path, target) => {
                let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
                let link = sandbox.temproot.join(name);
                let _ = fs::remove_file(&link); // May not exist
                std::os::unix::fs::symlink(target, &link)?;
            }
        }
    }
    // Create special directories
    fs::create_dir_all(sandbox.temproot.join("dev"))?;
    fs::create_dir_all(sandbox.temproot.join("proc"))?;
    fs::create_dir_all(sandbox.temproot.join("sys"))?;
    Ok(())
}

fn mount_overlays(sandbox: &Sandbox) -> Result<()> {
    // Check which top-level directory contains the sandbox (to skip circular mount)
    let sandbox_parent = sandbox.root.parent().and_then(|p| p.file_name());

    let entries = get_root_entries()?;
    for entry in &entries {
        if let RootEntry::Dir(path) = entry {
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");

            // Skip if sandbox is inside this directory (circular dependency)
            if sandbox_parent.map(|p| p == name).unwrap_or(false) {
                // For /tmp, just create a fresh tmpfs or empty dir
                let tmp_dir = sandbox.temproot.join(name);
                fs::create_dir_all(&tmp_dir)?;
                fs::set_permissions(&tmp_dir, Permissions::from_mode(0o1777))
                    .expect("Failed to set permissions");
                continue;
            }

            let options = format!(
                "userxattr,lowerdir={},upperdir={},workdir={}",
                path.display(),
                sandbox.upperdir.join(name).display(),
                sandbox.workdir.join(name).display()
            );
            if let Err(e) = mount(
                Some("overlay"),
                &sandbox.temproot.join(name),
                Some("overlay"),
                MsFlags::empty(),
                Some(options.as_str()),
            ) {
                eprintln!("Warning: failed to mount overlay for {}: {}", name, e);
            }
        }
    }

    // Mount /dev devices
    let dev_dir = sandbox.temproot.join("dev");
    for dev in &["null", "zero", "random", "urandom", "tty"] {
        let src = PathBuf::from("/dev").join(dev);
        let dst = dev_dir.join(dev);
        if src.exists() {
            fs::File::create(&dst).expect("Failed to create dev node");
            mount(
                Some(&src),
                &dst,
                None::<&str>,
                MsFlags::MS_BIND,
                None::<&str>,
            )
            .expect("Failed to bind mount dev");
        }
    }

    // Mount /proc
    let proc_dir = sandbox.temproot.join("proc");
    mount(
        None::<&str>,
        &proc_dir,
        Some("proc"),
        MsFlags::empty(),
        None::<&str>,
    )
    .expect("Failed to mount proc");

    Ok(())
}

fn exec_in_chroot(sandbox: &Sandbox, workdir: &Path, command: &str) -> ! {
    chroot(&sandbox.temproot).expect("Failed to chroot");
    env::set_current_dir("/").expect("Failed to chdir to /");
    env::set_current_dir(workdir).expect("Failed to chdir to workdir");

    let status = Command::new("/bin/sh").arg("-c").arg(command).status();

    match status {
        Ok(s) => std::process::exit(s.code().unwrap_or(1)),
        Err(e) => {
            eprintln!("Failed to execute: {}", e);
            std::process::exit(1);
        }
    }
}

struct CapturePaths {
    stdout_host: PathBuf,
    stderr_host: PathBuf,
}

impl CapturePaths {
    fn new(sandbox: &Sandbox) -> Result<Self> {
        let capture_dir = sandbox.temproot.join(".agfs-capture");
        fs::create_dir_all(&capture_dir).context("Failed to create capture directory")?;

        let stdout_host = capture_dir.join("stdout");
        let stderr_host = capture_dir.join("stderr");

        for path in [&stdout_host, &stderr_host] {
            if path.exists() {
                fs::remove_file(path)
                    .with_context(|| format!("Failed to clear capture file {}", path.display()))?;
            }
        }

        Ok(Self {
            stdout_host,
            stderr_host,
        })
    }

    fn stdout_in_chroot(&self) -> String {
        format!("{}/stdout", CAPTURE_DIR_IN_CHROOT)
    }

    fn stderr_in_chroot(&self) -> String {
        format!("{}/stderr", CAPTURE_DIR_IN_CHROOT)
    }

    fn collect(self, exit_code: i32) -> SandboxRunResult {
        let stdout = fs::read(&self.stdout_host).unwrap_or_default();
        let stderr = fs::read(&self.stderr_host).unwrap_or_default();

        SandboxRunResult {
            exit_code,
            stdout: String::from_utf8_lossy(&stdout).into_owned(),
            stderr: String::from_utf8_lossy(&stderr).into_owned(),
        }
    }
}

fn exec_in_chroot_capture(
    sandbox: &Sandbox,
    workdir: &Path,
    command: &str,
    capture: &CapturePaths,
) -> ! {
    chroot(&sandbox.temproot).expect("Failed to chroot");
    env::set_current_dir("/").expect("Failed to chdir to /");
    env::set_current_dir(workdir).expect("Failed to chdir to workdir");

    let stdout_path = capture.stdout_in_chroot();
    let stderr_path = capture.stderr_in_chroot();
    let stdout = File::create(&stdout_path).expect("Failed to open stdout capture");
    let stderr = File::create(&stderr_path).expect("Failed to open stderr capture");

    let status = Command::new("/bin/sh")
        .arg("-c")
        .arg(command)
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr))
        .status();

    match status {
        Ok(s) => std::process::exit(s.code().unwrap_or(1)),
        Err(e) => {
            let _ = fs::write(stderr_path, e.to_string());
            std::process::exit(1);
        }
    }
}

fn run_shell_command_in_sandbox_internal(
    sandbox: &Sandbox,
    workdir: &Path,
    command: &str,
    capture_output: bool,
) -> Result<SandboxRunResult> {
    // Ensure unprivileged user namespaces are allowed
    ensure_userns_allowed()?;

    let uid = nix::unistd::getuid();
    let gid = nix::unistd::getgid();
    let workdir = workdir.to_path_buf();
    let capture = if capture_output {
        Some(CapturePaths::new(sandbox)?)
    } else {
        None
    };

    // Prepare sandbox structure before forking
    prepare_sandbox(sandbox)?;

    let exit_code = match unsafe { fork() }? {
        ForkResult::Parent { child } => {
            let status = waitpid(child, None)?;
            unsafe {
                signal(SIGTTIN, SigHandler::SigIgn).unwrap();
                signal(SIGTTOU, SigHandler::SigIgn).unwrap();
                let stdin = std::io::stdin();
                let tty_fd = stdin.as_raw_fd();
                let pgid = getpgrp();
                tcsetpgrp(tty_fd, pgid);
                signal(SIGTTIN, SigHandler::SigDfl).unwrap();
                signal(SIGTTOU, SigHandler::SigDfl).unwrap();
            }
            match status {
                nix::sys::wait::WaitStatus::Exited(_, code) => code,
                _ => 1,
            }
        }
        ForkResult::Child => {
            // Create namespaces
            let flags =
                CloneFlags::CLONE_NEWUSER | CloneFlags::CLONE_NEWNS | CloneFlags::CLONE_NEWPID;

            unshare(flags).expect("Failed to unshare");

            // Map user to root
            fs::write("/proc/self/setgroups", "deny").expect("Failed to write setgroups");
            fs::write("/proc/self/uid_map", format!("0 {} 1\n", uid))
                .expect("Failed to write uid_map");
            fs::write("/proc/self/gid_map", format!("0 {} 1\n", gid))
                .expect("Failed to write gid_map");

            // Fork again to enter PID namespace (required for /proc mount)
            match unsafe { fork() } {
                Ok(ForkResult::Parent { child }) => {
                    let status = waitpid(child, None)
                        .unwrap_or(nix::sys::wait::WaitStatus::Exited(child, 1));
                    match status {
                        nix::sys::wait::WaitStatus::Exited(_, code) => std::process::exit(code),
                        _ => std::process::exit(1),
                    }
                }
                Ok(ForkResult::Child) => {
                    mount_overlays(sandbox).expect("Failed to mount");
                    if let Some(capture) = capture.as_ref() {
                        exec_in_chroot_capture(sandbox, &workdir, command, capture);
                    } else {
                        exec_in_chroot(sandbox, &workdir, command);
                    }
                }
                Err(e) => panic!("Failed to fork: {}", e),
            }
        }
    };

    Ok(match capture {
        Some(capture) => capture.collect(exit_code),
        None => SandboxRunResult {
            exit_code,
            stdout: String::new(),
            stderr: String::new(),
        },
    })
}

fn shell_command_from_args(command: &[String]) -> Result<String> {
    shlex::try_join(command.iter().map(String::as_str).collect::<Vec<_>>())
        .map_err(|err| anyhow::anyhow!("Failed to quote command: {err}"))
}

/// Run a command inside the sandbox.
pub fn run_in_sandbox(sandbox: &Sandbox, command: &[String]) -> Result<i32> {
    let command = shell_command_from_args(command)?;
    Ok(
        run_shell_command_in_sandbox_internal(sandbox, &env::current_dir()?, &command, false)?
            .exit_code,
    )
}

pub fn run_shell_command_in_sandbox(
    sandbox: &Sandbox,
    workdir: &Path,
    command: &str,
) -> Result<SandboxRunResult> {
    run_shell_command_in_sandbox_internal(sandbox, workdir, command, true)
}

fn prepare_dir_for_removal(path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    let file_type = metadata.file_type();

    if file_type.is_symlink() || !file_type.is_dir() {
        return Ok(());
    }

    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    for entry in fs::read_dir(path)? {
        prepare_dir_for_removal(&entry?.path())?;
    }

    Ok(())
}

pub fn destroy_sandbox(sandbox: &Sandbox) -> Result<()> {
    if sandbox.root.exists() {
        prepare_dir_for_removal(&sandbox.root)?;
        fs::remove_dir_all(&sandbox.root)?;
    }
    Ok(())
}

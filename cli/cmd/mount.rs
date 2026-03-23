// agfs CLI — mount.rs
//
// `agfs mount`    — create .agfs/ layout and mount the filesystem.
// `agfs unmount`  — unmount and clean up .agfs/.
// `agfs remount`  — unmount then mount again (picks up new agfs.toml options).

use crate::journal::Journal;
use anyhow::{Context, Result};
use colored::Colorize;
use std::env;
use std::fs;
use std::io::{self, BufRead, Write};
use std::os::unix;
use std::path::Path;

/// Enter a user + mount namespace (unprivileged). After this, the process
/// has CAP_SYS_ADMIN inside the namespace and can mount/pivot_root.
fn enter_namespace() -> Result<()> {
    let uid = nix::unistd::getuid();
    let gid = nix::unistd::getgid();

    // Create user + mount + PID namespace.
    // CLONE_NEWPID is needed so we can mount a fresh /proc inside the
    // namespace (the kernel requires a PID namespace for proc mounts).
    // Note: the calling process is NOT in the new PID namespace — its
    // next fork'd child will be PID 1 in it.
    nix::sched::unshare(
        nix::sched::CloneFlags::CLONE_NEWUSER
            | nix::sched::CloneFlags::CLONE_NEWNS
            | nix::sched::CloneFlags::CLONE_NEWPID,
    )
    .context("unshare(CLONE_NEWUSER | CLONE_NEWNS | CLONE_NEWPID)")?;

    // Write uid/gid maps: map real uid/gid 1:1 inside the namespace
    // Must deny setgroups first (kernel requirement for unprivileged)
    fs::write("/proc/self/setgroups", "deny").context("writing /proc/self/setgroups")?;
    fs::write("/proc/self/uid_map", format!("{uid} {uid} 1")).context("writing uid_map")?;
    fs::write("/proc/self/gid_map", format!("{gid} {gid} 1")).context("writing gid_map")?;

    // Make all mounts private so changes don't propagate to parent namespace
    nix::mount::mount(
        None::<&str>,
        "/",
        None::<&str>,
        nix::mount::MsFlags::MS_PRIVATE | nix::mount::MsFlags::MS_REC,
        None::<&str>,
    )
    .context("making mounts private")?;

    Ok(())
}

/// Try to unmount a path. If busy, show blocking processes and offer to kill them.
fn umount_or_prompt(target: &Path) -> Result<()> {
    use nix::errno::Errno;

    match nix::mount::umount(target) {
        Ok(()) => return Ok(()),
        Err(Errno::EBUSY) => {}
        Err(e) => anyhow::bail!("umount {}: {e}", target.display()),
    }

    // Unmount failed with EBUSY — find who's blocking it.
    let pids = get_blocking_pids(target);
    if pids.is_empty() {
        anyhow::bail!(
            "{} is busy (could not identify blocking processes)",
            target.display()
        );
    }

    eprintln!(
        "{} {} is busy, blocked by:",
        "agfs:".red(),
        target.display()
    );
    for &pid in &pids {
        let comm = fs::read_to_string(format!("/proc/{pid}/comm")).unwrap_or_default();
        eprintln!("  PID {pid}  {}", comm.trim());
    }

    eprint!("Kill these processes? [y/N] ");
    io::stderr().flush().ok();
    let mut input = String::new();
    io::stdin().lock().read_line(&mut input).ok();

    if !input.trim().eq_ignore_ascii_case("y") {
        anyhow::bail!(
            "{} is busy (user declined to kill blocking processes)",
            target.display()
        );
    }

    for &pid in &pids {
        let _ = nix::sys::signal::kill(
            nix::unistd::Pid::from_raw(pid as i32),
            nix::sys::signal::Signal::SIGTERM,
        );
    }
    std::thread::sleep(std::time::Duration::from_millis(200));

    nix::mount::umount(target).with_context(|| {
        format!(
            "umount {} after killing blocking processes",
            target.display()
        )
    })
}

/// Get PIDs of processes with open file descriptors on the same device as `mount_path`.
/// This is equivalent to `fuser -m` — walks /proc/<pid>/fd/ and compares device IDs.
fn get_blocking_pids(mount_path: &Path) -> Vec<u32> {
    use std::os::unix::fs::MetadataExt;

    let Ok(mount_meta) = fs::metadata(mount_path) else {
        return Vec::new();
    };
    let mount_dev = mount_meta.dev();
    let self_pid = std::process::id();

    let Ok(proc_entries) = fs::read_dir("/proc") else {
        return Vec::new();
    };

    let mut pids = Vec::new();
    for entry in proc_entries.flatten() {
        let pid: u32 = match entry.file_name().to_str().and_then(|s| s.parse().ok()) {
            Some(p) => p,
            None => continue,
        };
        if pid == self_pid {
            continue;
        }

        let fd_dir = entry.path().join("fd");
        let Ok(fds) = fs::read_dir(&fd_dir) else {
            continue;
        };
        let uses_mount = fds.flatten().any(|fd| {
            fs::metadata(fd.path())
                .map(|m| m.dev() == mount_dev)
                .unwrap_or(false)
        });
        if uses_mount {
            pids.push(pid);
        }
    }

    pids
}

/// Mount fresh pseudo filesystems into the agfs mount. Called in the
/// daemon's namespace before any exec does pivot_root, so they appear
/// at /proc, /sys, /dev inside the sandbox.
fn mount_pseudofs(mnt: &Path) -> Result<()> {
    // proc: requires CLONE_NEWPID (the daemon is PID 1 in the new ns).
    let proc_target = mnt.join("proc");
    if proc_target.exists() && !is_mountpoint(&proc_target) {
        nix::mount::mount(
            Some("proc"),
            &proc_target,
            Some("proc"),
            nix::mount::MsFlags::empty(),
            None::<&str>,
        )
        .with_context(|| format!("mounting proc on {}", proc_target.display()))?;
    }

    // dev: tmpfs + bind-mount individual device nodes from host.
    // devtmpfs can't be mounted in a user namespace, but individual
    // device files can be bind-mounted. Same approach as bubblewrap/podman.
    let dev_target = mnt.join("dev");
    if dev_target.exists() && !is_mountpoint(&dev_target) {
        nix::mount::mount(
            Some("tmpfs"),
            &dev_target,
            Some("tmpfs"),
            nix::mount::MsFlags::MS_NOSUID | nix::mount::MsFlags::MS_NOEXEC,
            Some("size=64k,mode=0755"),
        )
        .with_context(|| "mounting tmpfs on dev")?;

        // Bind-mount essential device nodes from host.
        const DEV_NODES: &[&str] = &["null", "zero", "full", "random", "urandom", "tty"];
        for name in DEV_NODES {
            let src = Path::new("/dev").join(name);
            let dst = dev_target.join(name);
            if !src.exists() {
                continue;
            }
            // Create the target file for bind-mount.
            fs::File::create(&dst).ok();
            if let Err(e) = nix::mount::mount(
                Some(&src),
                &dst,
                None::<&str>,
                nix::mount::MsFlags::MS_BIND,
                None::<&str>,
            ) {
                eprintln!("{} bind /dev/{name}: {e}", "agfs:".yellow());
                let _ = fs::remove_file(&dst);
            }
        }

        // Symlinks: /dev/stdin, /dev/stdout, /dev/stderr, /dev/fd
        let _ = unix::fs::symlink("/proc/self/fd", dev_target.join("fd"));
        let _ = unix::fs::symlink("/proc/self/fd/0", dev_target.join("stdin"));
        let _ = unix::fs::symlink("/proc/self/fd/1", dev_target.join("stdout"));
        let _ = unix::fs::symlink("/proc/self/fd/2", dev_target.join("stderr"));

        // /dev/shm and /dev/pts
        let shm = dev_target.join("shm");
        fs::create_dir_all(&shm).ok();
        let _ = nix::mount::mount(
            Some("tmpfs"),
            &shm,
            Some("tmpfs"),
            nix::mount::MsFlags::MS_NOSUID | nix::mount::MsFlags::MS_NODEV,
            Some("size=64m"),
        );
    }

    Ok(())
}

fn unmount_pseudofs(mnt: &Path) -> Result<()> {
    for dir in ["dev", "sys", "proc"] {
        let target = mnt.join(dir);
        if target.exists() && is_mountpoint(&target) {
            umount_or_prompt(&target).with_context(|| format!("unmounting {dir}"))?;
        }
    }
    Ok(())
}

/// Full teardown of an agfs session directory: unbind pseudofs, unmount, remove symlinks, clean up.
pub fn unmount_at(agfs_dir: &Path) -> Result<()> {
    let mnt = agfs_dir.join("mnt");

    // Remove symlinks first (they point into the mount)
    let _ = fs::remove_file(agfs_dir.join("cwd"));

    // Unbind pseudo filesystems, then unmount agfs
    unmount_pseudofs(&mnt)?;
    if mnt.exists() && is_mountpoint(&mnt) {
        umount_or_prompt(&mnt).with_context(|| format!("unmounting {}", mnt.display()))?;
    }

    // Remove the .agfs/ directory
    let _ = fs::remove_dir_all(agfs_dir);
    Ok(())
}

/// Create .agfs/ layout, enter a namespace, mount, apply rules, and stay
/// alive as a daemon holding the namespace. Other commands join via setns.
pub fn mount() -> Result<()> {
    let cwd = env::current_dir().context("getting cwd")?;
    let agfs_dir = cwd.join(".agfs");
    let mnt = agfs_dir.join("mnt");

    if mnt.exists() && is_mountpoint(&mnt) {
        let opts = crate::config::mount_options(&agfs_dir);
        eprintln!(
            "{} {} ({})",
            "agfs: mounted at".green(),
            mnt.display(),
            opts
        );
        return Ok(());
    }

    // Setup and load kmod on host (before entering namespace).
    setup_agfs_dir(&agfs_dir)?;
    super::load::load()?;

    // Fork first so the parent stays on the host. The child enters the
    // namespace, mounts agfs, and becomes the daemon. CLONE_NEWPID puts
    // the child (not the parent) into the new PID namespace, so the
    // child is PID 1 in it.
    match unsafe { libc::fork() } {
        -1 => anyhow::bail!("fork failed: {}", std::io::Error::last_os_error()),
        0 => {
            // Child: enter namespace, mount, become daemon.
            enter_namespace()?;
            do_mount(&agfs_dir)?;
            create_cwd_symlink(&agfs_dir, &cwd)?;
            crate::config::apply_rules(&agfs_dir)?;

            // Fork again for PID namespace: this grandchild is PID 1
            // inside the new PID namespace (needed for /proc mount).
            match unsafe { libc::fork() } {
                -1 => std::process::exit(1),
                0 => {
                    // Grandchild: daemon process (PID 1 in PID namespace).
                    mount_pseudofs(&mnt)?;

                    eprintln!("{} {}", "agfs: daemon ready".green(), mnt.display(),);

                    // Detach from parent's stdio so callers using
                    // Command::output() don't block.
                    unsafe {
                        let devnull = libc::open(b"/dev/null\0".as_ptr() as _, libc::O_RDWR);
                        if devnull >= 0 {
                            libc::dup2(devnull, 0);
                            libc::dup2(devnull, 1);
                            libc::dup2(devnull, 2);
                            libc::close(devnull);
                        }
                    }

                    // Signal readiness.
                    let ready_path = agfs_dir.join("ready");
                    let _ = fs::File::create(&ready_path);

                    wait_for_shutdown(&agfs_dir);
                    std::process::exit(0);
                }
                grandchild_pid => {
                    // Child (middle process): write grandchild's host PID
                    // and exit. The grandchild holds the namespace.
                    let pid_path = agfs_dir.join("pid");
                    let _ = fs::write(&pid_path, format!("{grandchild_pid}"));
                    std::process::exit(0);
                }
            }
        }
        child_pid => {
            // Parent: wait for the middle child to exit, then wait for
            // the daemon to signal readiness.
            unsafe {
                let mut status = 0i32;
                libc::waitpid(child_pid, &mut status, 0);
            }

            let ready_path = agfs_dir.join("ready");
            for _ in 0..500 {
                if ready_path.exists() {
                    let _ = fs::remove_file(&ready_path);
                    return Ok(());
                }
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            anyhow::bail!("daemon did not become ready in time");
        }
    }
}

/// Block until SIGTERM or SIGINT, then clean up.
fn wait_for_shutdown(agfs_dir: &Path) {
    use nix::sys::signal::{SigSet, Signal};

    // Block SIGTERM and SIGINT, then wait for either.
    let mut mask = SigSet::empty();
    mask.add(Signal::SIGTERM);
    mask.add(Signal::SIGINT);
    mask.thread_block().ok();

    // sigwait blocks until one of the masked signals arrives.
    let _ = mask.wait();

    eprintln!("{}", "agfs: shutting down".yellow());
    let _ = fs::remove_file(agfs_dir.join("pid"));
}

/// Unmount by signaling the daemon to shut down.
pub fn unmount(force: bool) -> Result<()> {
    let agfs_dir = crate::utils::session_dir()?;
    if !force {
        prompt_if_staged(&agfs_dir)?;
    }

    // Signal the daemon to shut down
    let pid_path = agfs_dir.join("pid");
    if pid_path.exists() {
        let pid_str = fs::read_to_string(&pid_path).context("reading .agfs/pid")?;
        if let Ok(pid) = pid_str.trim().parse::<i32>() {
            let _ = nix::sys::signal::kill(
                nix::unistd::Pid::from_raw(pid),
                nix::sys::signal::Signal::SIGTERM,
            );
            // Wait for daemon to exit. Use kill(pid, 0) to check if
            // the process still exists — faster than polling the pid file.
            for _ in 0..200 {
                if nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), None).is_err() {
                    break; // process gone
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        }
    }

    // Clean up any remaining state
    let _ = fs::remove_file(agfs_dir.join("cwd"));
    let _ = fs::remove_dir_all(&agfs_dir);

    eprintln!(
        "{} {}",
        "agfs: unmounted".green(),
        agfs_dir.join("mnt").display()
    );
    Ok(())
}

/// Unmount then mount again. Picks up new mount options from agfs.toml.
pub fn remount(force: bool) -> Result<()> {
    let agfs_dir = crate::utils::session_dir()?;
    if !force {
        prompt_if_staged(&agfs_dir)?;
    }
    unmount_at(&agfs_dir)?;
    mount()
}

/// If there are staged changes, ask the user to commit or abort before proceeding.
/// This runs from the host — no namespace access needed. Commit applies records
/// to the host filesystem; abort just discards (kernel state will be gone after
/// unmount anyway).
fn prompt_if_staged(agfs_dir: &Path) -> Result<()> {
    let journal = Journal::read(agfs_dir).unwrap_or_else(|_| Journal::new(vec![]));
    let tree = journal.into_tree();
    if tree.is_empty() {
        return Ok(());
    }

    eprintln!(
        "{}",
        format!(
            "Warning: {} staged change{} will be lost.",
            tree.len(),
            crate::utils::plural(tree.len())
        )
        .yellow()
        .bold()
    );
    eprint!(
        "{} ",
        "[c]ommit, [a]bort, or [q]uit? [default: quit]:".bold()
    );
    io::stderr().flush().ok();

    let mut line = String::new();
    io::stdin().lock().read_line(&mut line)?;

    match line.trim().to_ascii_lowercase().as_str() {
        "c" | "commit" => {
            // Apply to host only — no ioctl reset needed since we're
            // about to kill the daemon (kernel state goes away).
            let n = super::commit::apply_to_host(agfs_dir)?;
            if n > 0 {
                eprintln!(
                    "{}",
                    format!("Committed {n} change{}.", crate::utils::plural(n))
                        .green()
                        .bold()
                );
            }
        }
        "a" | "abort" => {} // just discard — daemon death cleans up
        _ => anyhow::bail!("unmount cancelled"),
    }
    Ok(())
}

/// Check if a path is a mount point by comparing device IDs with its parent.
fn is_mountpoint(path: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    let Ok(meta) = fs::metadata(path) else {
        return false;
    };
    let Ok(parent_meta) = fs::metadata(path.join("..")) else {
        return false;
    };
    meta.dev() != parent_meta.dev()
}

pub fn setup_agfs_dir(agfs_dir: &Path) -> Result<()> {
    fs::create_dir_all(agfs_dir.join("inodes")).context("creating .agfs/inodes/")?;
    fs::create_dir_all(agfs_dir.join("mnt")).context("creating .agfs/mnt/")?;

    // Create the journal file; the kernel module expects it to exist.
    let journal = agfs_dir.join("journal");
    if !journal.exists() {
        fs::File::create(&journal).context("creating .agfs/journal")?;
    }

    Ok(())
}

pub fn do_mount(agfs_dir: &Path) -> Result<()> {
    let mnt = agfs_dir.join("mnt");
    let mount_data = crate::config::mount_options(agfs_dir);
    let source = agfs_dir.to_string_lossy();

    eprintln!(
        "{} {} ({})",
        "agfs: mounting".green(),
        mnt.display(),
        mount_data
    );

    nix::mount::mount(
        Some(source.as_ref()),
        &mnt,
        Some("agfs"),
        nix::mount::MsFlags::empty(),
        Some(mount_data.as_str()),
    )
    .context("mounting agfs (is the kernel module loaded?)")?;

    Ok(())
}

/// Create .agfs/cwd symlink pointing to the cwd inside the mount.
fn create_cwd_symlink(agfs_dir: &Path, cwd: &Path) -> Result<()> {
    let link = agfs_dir.join("cwd");
    let target = agfs_dir
        .join("mnt")
        .join(cwd.strip_prefix("/").unwrap_or(cwd));
    if link.exists() || link.symlink_metadata().is_ok() {
        fs::remove_file(&link).context("removing old .agfs/cwd symlink")?;
    }
    unix::fs::symlink(&target, &link).context("creating .agfs/cwd symlink")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn is_mountpoint_returns_false_for_regular_dir() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(!is_mountpoint(tmp.path()));
    }

    #[test]
    fn is_mountpoint_returns_false_for_nonexistent() {
        assert!(!is_mountpoint(Path::new("/nonexistent_agfs_test_path")));
    }

    #[test]
    fn is_mountpoint_returns_true_for_proc() {
        // /proc is a mount point on Linux
        if Path::new("/proc/self").exists() {
            assert!(is_mountpoint(Path::new("/proc")));
        }
    }

    #[test]
    fn setup_agfs_dir_creates_layout() {
        let tmp = tempfile::tempdir().unwrap();
        let agfs = tmp.path().join(".agfs");
        setup_agfs_dir(&agfs).unwrap();
        assert!(agfs.join("inodes").is_dir());
        assert!(agfs.join("mnt").is_dir());
    }

    #[test]
    fn setup_agfs_dir_idempotent() {
        let tmp = tempfile::tempdir().unwrap();
        let agfs = tmp.path().join(".agfs");
        setup_agfs_dir(&agfs).unwrap();
        setup_agfs_dir(&agfs).unwrap(); // second call should not fail
        assert!(agfs.join("inodes").is_dir());
    }

    #[test]
    fn get_blocking_pids_finds_child_with_open_fd() {
        use std::process::{Command, Stdio};

        let tmp = tempfile::tempdir().unwrap();
        let file_path = tmp.path().join("held_open");
        fs::write(&file_path, "data").unwrap();

        // Spawn a child that holds the file open and sleeps.
        let child = Command::new("bash")
            .args(["-c", &format!("exec 3<'{}'; sleep 60", file_path.display())])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let child_pid = child.id();

        // Give the child a moment to open the fd.
        std::thread::sleep(std::time::Duration::from_millis(200));

        let pids = get_blocking_pids(tmp.path());
        assert!(
            pids.contains(&child_pid),
            "expected PID {child_pid} in blocking list, got {pids:?}"
        );

        // Clean up.
        let _ = nix::sys::signal::kill(
            nix::unistd::Pid::from_raw(child_pid as i32),
            nix::sys::signal::Signal::SIGTERM,
        );
    }

    #[test]
    fn get_blocking_pids_excludes_self() {
        // Our own PID should never appear even though we can stat files on the same device.
        let pids = get_blocking_pids(Path::new("/tmp"));
        let self_pid = std::process::id();
        assert!(
            !pids.contains(&self_pid),
            "self PID {self_pid} should be excluded, got {pids:?}"
        );
    }

    #[test]
    fn get_blocking_pids_returns_empty_for_nonexistent() {
        let pids = get_blocking_pids(Path::new("/nonexistent_agfs_test_path"));
        assert!(pids.is_empty());
    }

    #[test]
    fn create_cwd_symlink_creates_link() {
        let tmp = tempfile::tempdir().unwrap();
        let agfs = tmp.path().join(".agfs");
        fs::create_dir_all(agfs.join("mnt")).unwrap();
        let cwd = PathBuf::from("/some/work/dir");
        create_cwd_symlink(&agfs, &cwd).unwrap();

        let link = agfs.join("cwd");
        assert!(link.symlink_metadata().unwrap().file_type().is_symlink());
        let target = fs::read_link(&link).unwrap();
        assert_eq!(target, agfs.join("mnt/some/work/dir"));
    }

    #[test]
    fn create_cwd_symlink_replaces_existing() {
        let tmp = tempfile::tempdir().unwrap();
        let agfs = tmp.path().join(".agfs");
        fs::create_dir_all(agfs.join("mnt")).unwrap();

        create_cwd_symlink(&agfs, &PathBuf::from("/old/dir")).unwrap();
        create_cwd_symlink(&agfs, &PathBuf::from("/new/dir")).unwrap();

        let target = fs::read_link(agfs.join("cwd")).unwrap();
        assert_eq!(target, agfs.join("mnt/new/dir"));
    }
}

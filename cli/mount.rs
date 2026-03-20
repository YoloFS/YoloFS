// agfs CLI — mount.rs
//
// `agfs mount`    — create .agfs/ layout and mount the filesystem.
// `agfs unmount`  — unmount and clean up .agfs/.
// `agfs remount`  — unmount then mount again (picks up new agfs.toml options).

use anyhow::{Context, Result};
use colored::Colorize;
use std::env;
use std::fs;
use std::io::{self, BufRead, Write};
use std::os::unix;
use std::path::Path;

/// Bind-mount host pseudo filesystems into the agfs mount so they're visible inside the chroot.
const BIND_MOUNTS: &[&str] = &["/proc", "/sys", "/dev"];

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

fn bind_mount_pseudofs(mnt: &Path) -> Result<()> {
    for source in BIND_MOUNTS {
        let source_path = Path::new(source);
        if !source_path.exists() {
            continue;
        }
        let target = mnt.join(source.trim_start_matches('/'));
        if !target.exists() {
            continue;
        }
        if is_mountpoint(&target) {
            continue;
        }
        nix::mount::mount(
            Some(*source),
            &target,
            None::<&str>,
            nix::mount::MsFlags::MS_BIND,
            None::<&str>,
        )
        .with_context(|| format!("bind-mounting {source}"))?;
    }
    Ok(())
}

fn unbind_mount_pseudofs(mnt: &Path) -> Result<()> {
    for source in BIND_MOUNTS.iter().rev() {
        let target = mnt.join(source.trim_start_matches('/'));
        if target.exists() && is_mountpoint(&target) {
            umount_or_prompt(&target).with_context(|| format!("unbinding {source}"))?;
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
    unbind_mount_pseudofs(&mnt)?;
    if mnt.exists() && is_mountpoint(&mnt) {
        umount_or_prompt(&mnt).with_context(|| format!("unmounting {}", mnt.display()))?;
    }

    // Remove the .agfs/ directory
    let _ = fs::remove_dir_all(agfs_dir);
    Ok(())
}

/// Create .agfs/ layout, mount, and apply rules.
/// If already mounted, re-applies rules from agfs.toml.
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

    setup_agfs_dir(&agfs_dir)?;
    crate::kmod::load()?;
    do_mount(&agfs_dir)?;
    bind_mount_pseudofs(&mnt)?;
    create_cwd_symlink(&agfs_dir, &cwd)?;
    crate::config::apply_rules(&agfs_dir)?;
    Ok(())
}

/// Unmount the agfs filesystem and remove the .agfs/ directory.
pub fn unmount(force: bool) -> Result<()> {
    let agfs_dir = crate::utils::session_dir()?;
    if !force {
        prompt_if_staged(&agfs_dir)?;
    }
    unmount_at(&agfs_dir)?;
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
fn prompt_if_staged(agfs_dir: &Path) -> Result<()> {
    let records = crate::journal::read(agfs_dir).unwrap_or_default();
    if records.is_empty() {
        return Ok(());
    }

    eprintln!(
        "{}",
        "Warning: staged changes will be lost.".yellow().bold()
    );
    eprint!(
        "{} ",
        "[c]ommit, [a]bort, or [q]uit? [default: quit]:".bold()
    );
    io::stderr().flush().ok();

    let mut line = String::new();
    io::stdin().lock().read_line(&mut line)?;

    match line.trim().to_ascii_lowercase().as_str() {
        "c" | "commit" => crate::commit::run()?,
        "a" | "abort" => crate::abort::reset_staging(agfs_dir)?,
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

    // Chown .agfs/ and its contents to the real user. The CLI runs setuid
    // root, so dirs/files created above are root-owned. After exec drops
    // privileges, the real user needs write access to inodes/ (for staging
    // blobs) and journal (for appends).
    let uid = Some(nix::unistd::getuid());
    let gid = Some(nix::unistd::getgid());
    for path in [
        agfs_dir.to_path_buf(),
        agfs_dir.join("inodes"),
        agfs_dir.join("mnt"),
        journal,
    ] {
        nix::unistd::chown(&path, uid, gid).with_context(|| format!("chown {}", path.display()))?;
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

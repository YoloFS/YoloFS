// yolo CLI — mount.rs
//
// `yolo mount`    — create .yolofs/ layout and mount the filesystem.
// `yolo unmount`  — unmount and clean up .yolofs/.
// `yolo remount`  — unmount then mount again (picks up new yolofs.toml options).

use crate::journal::Journal;
use anyhow::{Context, Result};
use colored::Colorize;
use std::env;
use std::fs;
use std::io::{self, BufRead, Write};
use std::os::unix;
use std::path::Path;

/// Bind-mount host pseudo filesystems into the YoloFS mount so they're visible inside the chroot.
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
        "yolo:".red(),
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

/// Full teardown of a YoloFS session directory: unbind pseudofs, unmount, remove symlinks, clean up.
pub fn unmount_at(yolo_dir: &Path) -> Result<()> {
    let mnt = crate::utils::mnt_dir(yolo_dir);

    // Remove symlinks first (they point into the mount)
    let _ = fs::remove_file(yolo_dir.join("cwd"));

    // Unbind pseudo filesystems, then unmount YoloFS
    unbind_mount_pseudofs(&mnt)?;
    if mnt.exists() && is_mountpoint(&mnt) {
        umount_or_prompt(&mnt).with_context(|| format!("unmounting {}", mnt.display()))?;
    }

    // Remove the now-unmounted mountpoint dir (NOT its parent — that's the
    // runtime base shared with other sessions), then the in-workspace .yolofs/
    // (including its `mnt` convenience symlink).
    let _ = fs::remove_dir_all(&mnt);
    let _ = fs::remove_dir_all(yolo_dir);
    Ok(())
}

/// After mounting, remind the user to run `yolo watch` to answer permission
/// prompts. Without a watcher the kernel resolves `ask` paths to the ask default
/// immediately (no prompt, no hang) — so this is guidance, not an error.
fn hint_watch() {
    eprintln!(
        "{} run `yolo watch` to answer permission prompts",
        "yolo:".yellow()
    );
}

/// Create .yolofs/ layout, mount, and apply rules.
/// If already mounted, re-applies rules from yolofs.toml.
pub fn mount() -> Result<()> {
    let cwd = env::current_dir().context("getting cwd")?;
    let yolo_dir = cwd.join(".yolofs");
    let mnt = crate::utils::mnt_dir(&yolo_dir);

    if mnt.exists() && is_mountpoint(&mnt) {
        let opts = crate::config::mount_options(&yolo_dir);
        eprintln!(
            "{} {} ({})",
            "yolo: mounted at".green(),
            yolo_dir.join("mnt").display(),
            opts
        );
        hint_watch();
        return Ok(());
    }

    setup_yolo_dir(&yolo_dir)?;
    super::load::load()?;
    do_mount(&yolo_dir)?;

    // Everything below runs against a live mount. If any step fails, roll the
    // mount back — yolofs stacks over /, so a dangling mount makes ordinary
    // tools (cp, rm) walk the entire rootfs.
    let finish = bind_mount_pseudofs(&mnt)
        .and_then(|()| create_cwd_symlink(&yolo_dir, &cwd))
        .and_then(|()| crate::config::apply_rules(&yolo_dir));
    if let Err(e) = finish {
        let _ = unmount_at(&yolo_dir);
        return Err(e);
    }
    hint_watch();
    Ok(())
}

/// Unmount the yolofs filesystem and remove the .yolofs/ directory.
pub fn unmount(force: bool) -> Result<()> {
    let yolo_dir = crate::utils::session_dir()?;
    if !force {
        prompt_if_staged(&yolo_dir)?;
    }
    unmount_at(&yolo_dir)?;
    eprintln!(
        "{} {}",
        "yolo: unmounted".green(),
        yolo_dir.join("mnt").display()
    );
    Ok(())
}

/// Unmount then mount again. Picks up new mount options from yolofs.toml.
pub fn remount(force: bool) -> Result<()> {
    let yolo_dir = crate::utils::session_dir()?;
    if !force {
        prompt_if_staged(&yolo_dir)?;
    }
    unmount_at(&yolo_dir)?;
    mount()
}

/// If there are staged changes, ask the user to commit or abort before proceeding.
fn prompt_if_staged(yolo_dir: &Path) -> Result<()> {
    let journal = Journal::read(yolo_dir).unwrap_or_else(|_| Journal::new(vec![]));
    if !journal.has_staged_changes() {
        return Ok(());
    }

    eprintln!(
        "{}",
        "Warning: staged changes will be lost (run `yolo review` to see them)."
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
        "c" | "commit" => super::commit::run()?,
        "a" | "abort" => super::abort::reset_staging(yolo_dir)?,
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

pub fn setup_yolo_dir(yolo_dir: &Path) -> Result<()> {
    fs::create_dir_all(yolo_dir.join("inodes")).context("creating .yolofs/inodes/")?;

    // The mountpoint (an empty dir the over-`/` mount lands on) lives under the
    // per-user runtime dir, NOT in the workspace — so editors and indexers can't
    // wander into a recursive view of `/`. `.yolofs/mnt` is just a convenience
    // symlink to it (`cd .yolofs/mnt` still works for humans). On a fresh mount
    // we mkdtemp a unique location and record it in the symlink; on remount we
    // honor the recorded one so the location stays stable for the session.
    let mnt_link = yolo_dir.join("mnt");
    if mnt_link.symlink_metadata().is_ok() {
        let m = crate::utils::mnt_dir(yolo_dir);
        fs::create_dir_all(&m).with_context(|| format!("creating mountpoint {}", m.display()))?;
    } else {
        let m = crate::utils::create_mnt_dir()?;
        unix::fs::symlink(&m, &mnt_link).context("creating .yolofs/mnt symlink")?;
    }

    // Create the journal file; the kernel module expects it to exist.
    let journal = yolo_dir.join("journal");
    if !journal.exists() {
        fs::File::create(&journal).context("creating .yolofs/journal")?;
    }

    // No chown needed: the CLI carries capabilities rather than setuid root, so
    // it runs as the invoking user and everything above is created owned by them
    // — including the runtime mountpoint under their own `/run/user/<uid>`.

    Ok(())
}

pub fn do_mount(yolo_dir: &Path) -> Result<()> {
    let mnt = crate::utils::mnt_dir(yolo_dir);
    let mount_data = crate::config::mount_options(yolo_dir);
    let source = yolo_dir.to_string_lossy();

    // Show the in-workspace `.yolofs/mnt` symlink rather than the runtime
    // mountpoint path — it's the familiar, stable handle users interact with.
    eprintln!(
        "{} {} ({})",
        "yolo: mounting".green(),
        yolo_dir.join("mnt").display(),
        mount_data
    );

    nix::mount::mount(
        Some(source.as_ref()),
        &mnt,
        Some("yolofs"),
        nix::mount::MsFlags::empty(),
        Some(mount_data.as_str()),
    )
    .context("mounting YoloFS (is the kernel module loaded?)")?;

    Ok(())
}

/// Create .yolofs/cwd symlink pointing to the cwd inside the mount.
fn create_cwd_symlink(yolo_dir: &Path, cwd: &Path) -> Result<()> {
    let link = yolo_dir.join("cwd");
    let target = crate::utils::mnt_dir(yolo_dir).join(cwd.strip_prefix("/").unwrap_or(cwd));
    if link.exists() || link.symlink_metadata().is_ok() {
        fs::remove_file(&link).context("removing old .yolofs/cwd symlink")?;
    }
    unix::fs::symlink(&target, &link).context("creating .yolofs/cwd symlink")?;
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
        assert!(!is_mountpoint(Path::new("/nonexistent_yolo_test_path")));
    }

    #[test]
    fn is_mountpoint_returns_true_for_proc() {
        // /proc is a mount point on Linux
        if Path::new("/proc/self").exists() {
            assert!(is_mountpoint(Path::new("/proc")));
        }
    }

    /// Create `.yolofs/` with its `mnt` symlink already pointing at a tempdir, so
    /// `setup_yolo_dir` honors that recorded location instead of reaching for the
    /// real `/run/user/<uid>` runtime dir — keeps these tests hermetic.
    fn yolofs_with_recorded_mnt(tmp: &Path) -> (PathBuf, PathBuf) {
        let yolofs = tmp.join(".yolofs");
        fs::create_dir_all(&yolofs).unwrap();
        let mnt = tmp.join("realmnt");
        unix::fs::symlink(&mnt, yolofs.join("mnt")).unwrap();
        (yolofs, mnt)
    }

    #[test]
    fn setup_yolo_dir_creates_layout() {
        let tmp = tempfile::tempdir().unwrap();
        let (yolofs, mnt) = yolofs_with_recorded_mnt(tmp.path());
        setup_yolo_dir(&yolofs).unwrap();
        assert!(yolofs.join("inodes").is_dir());
        // `.yolofs/mnt` is a symlink to the (now created) mountpoint dir.
        assert!(
            yolofs
                .join("mnt")
                .symlink_metadata()
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert!(mnt.is_dir());
    }

    #[test]
    fn setup_yolo_dir_idempotent() {
        let tmp = tempfile::tempdir().unwrap();
        let (yolofs, _mnt) = yolofs_with_recorded_mnt(tmp.path());
        setup_yolo_dir(&yolofs).unwrap();
        setup_yolo_dir(&yolofs).unwrap(); // second call should not fail
        assert!(yolofs.join("inodes").is_dir());
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
        let pids = get_blocking_pids(Path::new("/nonexistent_yolo_test_path"));
        assert!(pids.is_empty());
    }

    /// Point `.yolofs/mnt` at a real tempdir so `mnt_dir` resolves it without
    /// touching `/run/user/<uid>` — keeps these symlink tests hermetic.
    fn fake_mnt(tmp: &Path) -> (PathBuf, PathBuf) {
        let yolofs = tmp.join(".yolofs");
        fs::create_dir_all(&yolofs).unwrap();
        let mnt = tmp.join("realmnt");
        fs::create_dir_all(&mnt).unwrap();
        unix::fs::symlink(&mnt, yolofs.join("mnt")).unwrap();
        (yolofs, mnt)
    }

    #[test]
    fn create_cwd_symlink_creates_link() {
        let tmp = tempfile::tempdir().unwrap();
        let (yolofs, mnt) = fake_mnt(tmp.path());
        create_cwd_symlink(&yolofs, &PathBuf::from("/some/work/dir")).unwrap();

        let link = yolofs.join("cwd");
        assert!(link.symlink_metadata().unwrap().file_type().is_symlink());
        let target = fs::read_link(&link).unwrap();
        assert_eq!(target, mnt.join("some/work/dir"));
    }

    #[test]
    fn create_cwd_symlink_replaces_existing() {
        let tmp = tempfile::tempdir().unwrap();
        let (yolofs, mnt) = fake_mnt(tmp.path());

        create_cwd_symlink(&yolofs, &PathBuf::from("/old/dir")).unwrap();
        create_cwd_symlink(&yolofs, &PathBuf::from("/new/dir")).unwrap();

        let target = fs::read_link(yolofs.join("cwd")).unwrap();
        assert_eq!(target, mnt.join("new/dir"));
    }
}

// yolo CLI — mount.rs
//
// `yolo mount`    — create .yolofs/ layout and mount the filesystem.
// `yolo unmount`  — unmount the live view, preserving .yolofs/.
// `yolo remount`  — unmount then mount again (picks up new yolofs.toml options).

use crate::ioctl;
use crate::journal::Journal;
use crate::report;
use anyhow::{Context, Result};
use std::env;
use std::fs;
use std::io::{self, BufRead};
use std::os::unix;
use std::path::Path;

/// Try to unmount a path. If busy, show blocking processes and offer to kill them.
pub(crate) fn umount_or_prompt(target: &Path) -> Result<()> {
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

    report::warn(format!("{} is busy, blocked by:", target.display()));
    for &pid in &pids {
        let comm = fs::read_to_string(format!("/proc/{pid}/comm")).unwrap_or_default();
        report::detail(format!("PID {pid}  {}", comm.trim()));
    }

    report::prompt("kill these processes? [y/N]:");
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

/// Tear down the live view while preserving the durable `.yolofs/` artifact.
pub fn unmount_at(yolo_dir: &Path) -> Result<()> {
    let mnt = crate::utils::mnt_dir(yolo_dir);

    // Remove symlinks first (they point into the mount)
    let _ = fs::remove_file(yolo_dir.join("cwd"));

    // `/proc` `/sys` `/dev` are mounted per-command inside each `yolo run`'s
    // private namespace (see run.rs), not here, so there is nothing to unbind —
    // just unmount the yolofs view itself.
    if mnt.exists() && is_mountpoint(&mnt) {
        umount_or_prompt(&mnt).with_context(|| format!("unmounting {}", mnt.display()))?;
    }

    // Remove the now-unmounted runtime mountpoint, but preserve `.yolofs/`.
    let _ = fs::remove_dir_all(&mnt);
    Ok(())
}

/// After mounting, remind the user to run `yolo watch` to answer permission
/// prompts. Without a watcher the kernel resolves `ask` paths to the ask default
/// immediately (no prompt, no hang) — so this is guidance, not an error.
fn hint_watch() {
    report::hint("run `yolo watch` to answer permission prompts");
}

/// Create .yolofs/ layout, mount, and apply rules.
/// If already mounted, re-applies rules from yolofs.toml.
pub fn mount() -> Result<()> {
    let cwd = env::current_dir().context("getting cwd")?;
    let yolo_dir = cwd.join(".yolofs");
    let mnt = crate::utils::mnt_dir(&yolo_dir);

    if mnt.exists() && is_mountpoint(&mnt) {
        report::hint(format!(
            "already mounted at {}",
            yolo_dir.join("mnt").display()
        ));
        hint_watch();
        return Ok(());
    }

    let artifact_existed = yolo_dir.join("journal").exists();
    setup_yolo_dir(&yolo_dir)?;
    super::load::load()?;
    do_mount(&yolo_dir)?;

    let restored_staging = if artifact_existed {
        match restore_artifact(&yolo_dir) {
            Ok(restored) => restored,
            Err(e) => {
                let _ = unmount_at(&yolo_dir);
                return Err(e).context(
                    "staged changes could not be restored — `yolo review`/`commit`/`abort` work without mounting",
                );
            }
        }
    } else {
        false
    };

    // Everything below runs against a live mount. If any step fails, roll the
    // mount back — yolofs stacks over /, so a dangling mount makes ordinary
    // tools (cp, rm) walk the entire rootfs.
    let finish =
        create_cwd_symlink(&yolo_dir, &cwd).and_then(|()| crate::config::apply_rules(&yolo_dir));
    if let Err(e) = finish {
        let _ = unmount_at(&yolo_dir);
        return Err(e);
    }
    report::success(format!("mounted {}", yolo_dir.join("mnt").display()));
    if restored_staging {
        report::info("restored staged changes from a previous session — `yolo review` to inspect");
    }
    hint_watch();
    Ok(())
}

/// Return the current session, mounting on demand for the overlay exec path.
pub fn ensure_mounted() -> Result<std::path::PathBuf> {
    let cwd = env::current_dir().context("getting cwd")?;
    let yolo_dir = cwd.join(".yolofs");
    let mnt = crate::utils::mnt_dir(&yolo_dir);
    if is_mountpoint(&mnt) {
        return Ok(yolo_dir);
    }
    if !is_yolofs_project(&cwd) {
        anyhow::bail!(
            "not a yolofs project — run `yolo init` first (or `yolo mount` to mount anyway)"
        );
    }
    report::info("not mounted — mounting now (run `yolo unmount` when you're done)");
    mount()?;
    Ok(yolo_dir)
}

fn is_yolofs_project(cwd: &Path) -> bool {
    cwd.join("yolofs.toml").exists()
}

fn restore_artifact(yolo_dir: &Path) -> Result<bool> {
    let journal = Journal::read(yolo_dir)?;
    let has_staged_changes = journal.has_staged_changes;
    let latest_gen = journal.latest_gen;
    let dirty = journal.dirty;
    let alloc_ino_floor = journal.alloc_ino_floor;
    let cow_ino_floor = journal.cow_ino_floor;
    let tree = journal.into_tree().serialize();
    let ctl_file = ioctl::open(yolo_dir).context("opening ctl for restore")?;
    ioctl::restore(
        &ctl_file,
        latest_gen,
        dirty,
        alloc_ino_floor,
        cow_ino_floor,
        &tree,
    )?;
    Ok(has_staged_changes)
}

/// Unmount the live view. The durable artifact is always preserved.
pub fn unmount() -> Result<()> {
    let yolo_dir = crate::utils::session_dir()?;
    let mnt = crate::utils::mnt_dir(&yolo_dir);
    let was_mounted = is_mountpoint(&mnt);
    unmount_at(&yolo_dir)?;
    if was_mounted {
        report::success(format!("unmounted {}", yolo_dir.join("mnt").display()));
    } else {
        report::hint("view already unmounted");
    }
    Ok(())
}

/// Unmount then mount again. Picks up new mount options from yolofs.toml.
pub fn remount() -> Result<()> {
    let yolo_dir = crate::utils::session_dir()?;
    unmount_at(&yolo_dir)?;
    mount()
}

/// Check if a path is a mount point by comparing device IDs with its parent.
pub(crate) fn is_mountpoint(path: &Path) -> bool {
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

    #[test]
    fn project_marker_is_config_only() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(!is_yolofs_project(tmp.path()));
        std::fs::write(tmp.path().join("yolofs.toml"), "").unwrap();
        assert!(is_yolofs_project(tmp.path()));
        std::fs::remove_file(tmp.path().join("yolofs.toml")).unwrap();
        std::fs::create_dir(tmp.path().join(".yolofs")).unwrap();
        assert!(!is_yolofs_project(tmp.path()));
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

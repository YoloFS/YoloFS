// agfs CLI — mount.rs
//
// `agfs mount`    — create .agfs/ layout and mount the filesystem.
// `agfs unmount`  — unmount and clean up .agfs/.
// `agfs remount`  — unmount then mount again (picks up new agfs.toml options).

use anyhow::{Context, Result};
use colored::Colorize;
use std::env;
use std::fs;
use std::os::unix;
use std::path::Path;

/// Bind-mount host pseudo filesystems into the agfs mount so they're visible inside the chroot.
const BIND_MOUNTS: &[&str] = &["/proc", "/sys", "/dev"];

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
        eprintln!("{} {}", "agfs: bind-mounting".green(), source);
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

fn unbind_mount_pseudofs(mnt: &Path) {
    for source in BIND_MOUNTS.iter().rev() {
        let target = mnt.join(source.trim_start_matches('/'));
        if target.exists() && is_mountpoint(&target) {
            eprintln!("{} {}", "agfs: unbinding".green(), source);
            let _ = nix::mount::umount(&target);
        }
    }
}

/// Full teardown of an agfs session directory: unbind pseudofs, unmount, remove symlinks, clean up.
pub fn unmount_at(agfs_dir: &Path) {
    let mnt = agfs_dir.join("mnt");

    // Remove symlinks first (they point into the mount)
    let _ = fs::remove_file(agfs_dir.join("cwd"));

    // Unbind pseudo filesystems, then unmount agfs
    unbind_mount_pseudofs(&mnt);
    let _ = nix::mount::umount(&mnt);

    // Remove the .agfs/ directory
    let _ = fs::remove_dir_all(agfs_dir);
}

/// Create .agfs/ layout, mount, and apply rules.
/// If already mounted, re-applies rules from agfs.toml.
pub fn mount() -> Result<()> {
    let cwd = env::current_dir().context("getting cwd")?;
    let agfs_dir = cwd.join(".agfs");
    let mnt = agfs_dir.join("mnt");

    if mnt.exists() && is_mountpoint(&mnt) {
        eprintln!("{} {}", "agfs: mounted at".green(), mnt.display());
        return Ok(());
    }

    setup_agfs_dir(&agfs_dir)?;
    do_mount(&agfs_dir)?;
    bind_mount_pseudofs(&mnt)?;
    create_cwd_symlink(&agfs_dir, &cwd)?;
    crate::config::apply_rules(&agfs_dir)?;
    Ok(())
}

/// Unmount the agfs filesystem and remove the .agfs/ directory.
pub fn unmount() -> Result<()> {
    let agfs_dir = crate::session_dir()?;
    unmount_at(&agfs_dir);
    eprintln!("{} {}", "agfs: unmounted".green(), agfs_dir.join("mnt").display());
    Ok(())
}

/// Unmount then mount again. Picks up new mount options from agfs.toml.
pub fn remount() -> Result<()> {
    unmount()?;
    mount()
}

/// Check if a path is a mount point by comparing device IDs with its parent.
fn is_mountpoint(path: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    let Ok(meta) = fs::metadata(path) else { return false };
    let Ok(parent_meta) = fs::metadata(path.join("..")) else { return false };
    meta.dev() != parent_meta.dev()
}

pub fn setup_agfs_dir(agfs_dir: &Path) -> Result<()> {
    fs::create_dir_all(agfs_dir.join("staging"))
        .context("creating .agfs/staging/")?;
    fs::create_dir_all(agfs_dir.join("mnt"))
        .context("creating .agfs/mnt/")?;
    Ok(())
}

pub fn do_mount(agfs_dir: &Path) -> Result<()> {
    let mnt = agfs_dir.join("mnt");
    let mount_data = crate::config::mount_options(agfs_dir);
    let source = agfs_dir.to_string_lossy();

    eprintln!("{} {}", "agfs: mounting".green(), mnt.display());

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
    let target = agfs_dir.join("mnt").join(cwd.strip_prefix("/").unwrap_or(cwd));
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
        assert!(agfs.join("staging").is_dir());
        assert!(agfs.join("mnt").is_dir());
    }

    #[test]
    fn setup_agfs_dir_idempotent() {
        let tmp = tempfile::tempdir().unwrap();
        let agfs = tmp.path().join(".agfs");
        setup_agfs_dir(&agfs).unwrap();
        setup_agfs_dir(&agfs).unwrap(); // second call should not fail
        assert!(agfs.join("staging").is_dir());
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

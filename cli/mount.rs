// agfs CLI — mount.rs
//
// `agfs mount` — create .agfs/ layout and mount the filesystem.

use anyhow::{Context, Result};
use colored::Colorize;
use std::env;
use std::fs;
use std::os::unix;
use std::path::{Path, PathBuf};

/// Create .agfs/ layout, mount, and apply rules.
pub fn run() -> Result<()> {
    let cwd = env::current_dir().context("getting cwd")?;
    let agfs_dir = agfs_dir_path()?;
    let mnt = agfs_dir.join("mnt");

    if mnt.exists() && is_mountpoint(&mnt) {
        eprintln!("{} {}", "agfs: mounted at".green(), mnt.display());
        return Ok(());
    }

    setup_agfs_dir(&agfs_dir)?;
    do_mount(&agfs_dir)?;
    create_cwd_symlink(&agfs_dir, &cwd)?;
    crate::config::apply_rules(&agfs_dir)?;
    Ok(())
}

/// Return the .agfs/ path for the current directory.
pub fn agfs_dir_path() -> Result<PathBuf> {
    let cwd = env::current_dir().context("getting cwd")?;
    Ok(cwd.join(".agfs"))
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

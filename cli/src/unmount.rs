// agfs CLI — unmount.rs
//
// Unmount the agfs filesystem and remove the .agfs/ directory.

use anyhow::{Context, Result};
use colored::Colorize;
use std::fs;
use std::path::Path;

/// Check if a path is a mount point by comparing device IDs with its parent.
fn is_mountpoint(path: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    let Ok(meta) = fs::metadata(path) else { return false };
    let Ok(parent_meta) = fs::metadata(path.join("..")) else { return false };
    meta.dev() != parent_meta.dev()
}

pub fn run() -> Result<()> {
    let agfs_dir = crate::session_dir()?;
    let mnt = agfs_dir.join("mnt");

    // Remove symlinks first (they point into the mount)
    let cwd_link = agfs_dir.join("cwd");
    if cwd_link.symlink_metadata().is_ok() {
        fs::remove_file(&cwd_link).context("removing cwd symlink")?;
    }

    // Unmount pseudo-filesystems first (children must go before parent).
    // Only unmount if actually a mount point (mount skips dirs that don't exist).
    for pseudo in &["sys", "proc", "dev"] {
        let target = mnt.join(pseudo);
        if is_mountpoint(&target) {
            nix::mount::umount(&target)
                .with_context(|| format!("unmounting {pseudo}"))?;
        }
    }

    // Unmount agfs itself
    nix::mount::umount(&mnt)
        .context("unmounting .agfs/mnt")?;

    fs::remove_dir_all(&agfs_dir)
        .context("removing .agfs/ directory")?;

    eprintln!("{}", "agfs: session cleaned up".cyan());
    Ok(())
}

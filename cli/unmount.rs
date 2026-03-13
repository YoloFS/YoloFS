// agfs CLI — unmount.rs
//
// Unmount the agfs filesystem and remove the .agfs/ directory.

use anyhow::{Context, Result};
use colored::Colorize;
use std::fs;

pub fn run() -> Result<()> {
    let agfs_dir = crate::session_dir()?;
    let mnt = agfs_dir.join("mnt");

    // Remove symlinks first (they point into the mount)
    let cwd_link = agfs_dir.join("cwd");
    if cwd_link.symlink_metadata().is_ok() {
        fs::remove_file(&cwd_link).context("removing cwd symlink")?;
    }

    // Unmount agfs (ignore EINVAL — means it wasn't mounted)
    match nix::mount::umount(&mnt) {
        Ok(()) => {}
        Err(nix::errno::Errno::EINVAL) => {}
        Err(e) => return Err(e).context("unmounting .agfs/mnt"),
    }

    fs::remove_dir_all(&agfs_dir)
        .context("removing .agfs/ directory")?;

    eprintln!("{} {}", "agfs: unmounted".green(), mnt.display());
    Ok(())
}

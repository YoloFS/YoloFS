// agfs CLI — unmount.rs
//
// Unmount the agfs filesystem and remove the .agfs/ directory.

use anyhow::{Context, Result};
use colored::Colorize;
use std::fs;

pub fn run() -> Result<()> {
    let agfs_dir = crate::session_dir()?;
    let mnt = agfs_dir.join("mnt");

    // Remove symlinks first (they point into the mount and may hold references)
    let cwd_link = agfs_dir.join("cwd");
    if cwd_link.symlink_metadata().is_ok() {
        let _ = fs::remove_file(&cwd_link);
    }

    // Unmount pseudo-filesystems (reverse order)
    for pseudo in &["sys", "proc", "dev"] {
        let target = mnt.join(pseudo);
        if nix::mount::umount(&target).is_err() {
            let _ = nix::mount::umount2(&target, nix::mount::MntFlags::MNT_DETACH);
        }
    }

    // Unmount agfs itself
    if nix::mount::umount(&mnt).is_err() {
        nix::mount::umount2(&mnt, nix::mount::MntFlags::MNT_DETACH)
            .context("unmounting .agfs/mnt")?;
    }

    fs::remove_dir_all(&agfs_dir)
        .context("removing .agfs/ directory")?;

    eprintln!("{}", "agfs: session cleaned up".cyan());
    Ok(())
}

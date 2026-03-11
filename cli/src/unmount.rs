// agfs CLI — unmount.rs
//
// Unmount the agfs filesystem and remove the .agfs/ directory.

use anyhow::{Context, Result};
use colored::Colorize;
use std::fs;

pub fn run() -> Result<()> {
    let agfs_dir = crate::ctl::agfs_dir()?;
    let mnt = agfs_dir.join("mnt");

    // Unmount pseudo-filesystems first (reverse order, non-lazy)
    for pseudo in &["sys", "proc", "dev"] {
        let target = mnt.join(pseudo);
        // Try regular unmount first, fall back to lazy
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

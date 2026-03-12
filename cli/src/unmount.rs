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

    // Unmount pseudo-filesystems first (children must go before parent)
    for pseudo in &["sys", "proc", "dev"] {
        let target = mnt.join(pseudo);
        nix::mount::umount(&target)
            .with_context(|| format!("unmounting {pseudo}"))?;
    }

    // Unmount agfs itself
    nix::mount::umount(&mnt)
        .context("unmounting .agfs/mnt")?;

    fs::remove_dir_all(&agfs_dir)
        .context("removing .agfs/ directory")?;

    eprintln!("{}", "agfs: session cleaned up".cyan());
    Ok(())
}

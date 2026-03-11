// agfs CLI — unmount.rs
//
// Unmount the agfs filesystem and remove the .agfs/ directory.

use anyhow::{Context, Result};
use std::fs;

pub fn run() -> Result<()> {
    let agfs_dir = crate::ctl::agfs_dir()?;
    let mnt = agfs_dir.join("mnt");

    nix::mount::umount2(&mnt, nix::mount::MntFlags::MNT_DETACH)
        .context("unmounting .agfs/mnt")?;

    fs::remove_dir_all(&agfs_dir)
        .context("removing .agfs/ directory")?;

    eprintln!("agfs: session cleaned up");
    Ok(())
}

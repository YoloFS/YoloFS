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

    nix::mount::mount(
        Some("none"),
        &mnt,
        Some("agfs"),
        nix::mount::MsFlags::empty(),
        Some(mount_data.as_str()),
    )
    .context("mounting agfs (is the kernel module loaded?)")?;

    // Mount fresh pseudo-filesystems so they bypass agfs
    for &(dir, fstype) in &[("dev", "devtmpfs"), ("proc", "proc"), ("sys", "sysfs")] {
        let target = mnt.join(dir);
        if target.exists() {
            nix::mount::mount(
                Some(fstype),
                &target,
                Some(fstype),
                nix::mount::MsFlags::empty(),
                None::<&str>,
            )
            .with_context(|| format!("mounting {fstype} at {dir}"))?;
        }
    }

    eprintln!("{} {}", "agfs: mounted at".green(), mnt.display());
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

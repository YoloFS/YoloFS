// agfs CLI — abort.rs
//
// `agfs abort` — discard staged changes (§3.6).

use crate::{ioctl, unmount};
use anyhow::{Context, Result};
use colored::Colorize;
use std::fs;

pub fn run() -> Result<()> {
    let agfs = crate::session_dir()?;
    if !agfs.exists() {
        anyhow::bail!("no agfs session found (no .agfs/ directory)");
    }

    let staging_dir = agfs.join("staging");
    let renames_path = agfs.join("renames");

    // rm -rf staging/
    if staging_dir.exists() {
        fs::remove_dir_all(&staging_dir)
            .context("removing staging directory")?;
        fs::create_dir_all(&staging_dir)
            .context("recreating staging directory")?;
    }

    // rm renames
    if renames_path.exists() {
        fs::remove_file(&renames_path)
            .context("removing renames file")?;
    }

    // Signal kernel to invalidate caches
    let ctl_file = ioctl::open(&agfs).context("opening ctl for cache invalidation")?;
    ioctl::invalidate_cache(&ctl_file).context("invalidating cache")?;

    println!("{}", "Staging discarded.".yellow().bold());

    // Unmount after abort
    unmount::run()?;

    Ok(())
}

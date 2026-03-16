// agfs CLI — abort.rs
//
// `agfs abort` — discard staged changes (§3.6).

use anyhow::{Context, Result};
use colored::Colorize;
use std::fs;
use std::io::{self, BufRead, Write};
use std::path::Path;

/// Clear inode store, remove journal, and invalidate kernel caches.
pub fn reset_staging(agfs: &Path) -> Result<()> {
    let inodes_dir = agfs.join("inodes");
    if inodes_dir.exists() {
        fs::remove_dir_all(&inodes_dir).context("removing inode store")?;
        fs::create_dir_all(&inodes_dir).context("recreating inode store")?;
    }
    let journal_path = agfs.join("journal");
    if journal_path.exists() {
        fs::remove_file(&journal_path).context("removing journal file")?;
    }
    let ctl_file = crate::ioctl::open(agfs).context("opening ctl for cache invalidation")?;
    crate::ioctl::invalidate_cache(&ctl_file).context("invalidating cache")?;
    Ok(())
}

pub fn run(force: bool) -> Result<()> {
    let agfs = crate::utils::session_dir()?;

    let changes = crate::journal::resolve(&agfs)?;
    if changes.is_empty() {
        println!("{}", "Nothing to discard.".yellow());
        return Ok(());
    }

    if !force {
        eprint!(
            "{} ",
            format!(
                "Discard {} staged change{}? [y/N]:",
                changes.len(),
                crate::utils::plural(changes.len())
            )
            .bold()
        );
        io::stderr().flush().ok();

        let mut line = String::new();
        io::stdin().lock().read_line(&mut line)?;
        if !line.trim().eq_ignore_ascii_case("y") {
            eprintln!("{}", "Abort cancelled.".dimmed());
            return Ok(());
        }
    }

    crate::abort::reset_staging(&agfs)?;

    println!("{}", "Staging discarded.".yellow().bold());

    Ok(())
}

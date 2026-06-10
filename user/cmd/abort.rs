// yolo CLI — abort.rs
//
// `yolo abort` — discard staged changes.

use crate::journal::Journal;
use crate::report;
use anyhow::{Context, Result};
use std::fs;
use std::io::{self, BufRead};
use std::path::Path;

/// Clear inode store, truncate journal, and reset kernel staging state.
pub fn reset_staging(yolofs: &Path) -> Result<()> {
    let inodes_dir = yolofs.join("inodes");
    if inodes_dir.exists() {
        // Remove each shard subdirectory but keep the inodes/ directory itself.
        // The kernel module caches the inodes_dir dentry at mount time, so
        // deleting and recreating the directory would invalidate that cache.
        for entry in fs::read_dir(&inodes_dir).context("reading inode store")? {
            let path = entry.context("reading directory entry")?.path();
            fs::remove_dir_all(&path).context("removing shard directory")?;
        }
    }
    let journal_path = yolofs.join("journal");
    if journal_path.exists() {
        // OpenOptions with truncate(true) clears the file to zero length
        // while keeping the inode, so the kernel's O_APPEND fd stays valid.
        fs::OpenOptions::new()
            .write(true)
            .truncate(true)
            .open(&journal_path)
            .context("truncating journal")?;
    }
    let ctl_file = crate::ioctl::open(yolofs).context("opening ctl for reset")?;
    crate::ioctl::reset(&ctl_file).context("ioctl RESET")?;
    Ok(())
}

pub fn run(force: bool) -> Result<()> {
    let yolofs = crate::utils::session_dir()?;

    let journal = Journal::read(&yolofs)?;
    if !journal.has_staged_changes() {
        report::hint("nothing to discard");
        return Ok(());
    }

    if !force {
        report::prompt("discard all staged changes? (`yolo review` to see them) [y/N]:");

        let mut line = String::new();
        io::stdin().lock().read_line(&mut line)?;
        if !line.trim().eq_ignore_ascii_case("y") {
            report::hint("abort cancelled");
            return Ok(());
        }
    }

    reset_staging(&yolofs)?;

    report::success("staging discarded");

    Ok(())
}

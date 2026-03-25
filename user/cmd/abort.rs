// agfs CLI — abort.rs
//
// `agfs abort` — discard staged changes.

use crate::journal::Journal;
use anyhow::{Context, Result};
use colored::Colorize;
use std::fs;
use std::io::{self, BufRead, Write};
use std::path::Path;

/// Clear inode store, truncate journal, and reset kernel staging state.
pub fn reset_staging(agfs: &Path) -> Result<()> {
    let inodes_dir = agfs.join("inodes");
    if inodes_dir.exists() {
        for entry in fs::read_dir(&inodes_dir).context("reading inode store")? {
            let entry = entry.context("reading directory entry")?;
            let path = entry.path();
            if entry.file_type().context("reading file type")?.is_dir() {
                fs::remove_dir_all(&path).context("removing staged directory")?;
            } else {
                fs::remove_file(&path).context("removing staged inode")?;
            }
        }
    }
    let journal_path = agfs.join("journal");
    if journal_path.exists() {
        // OpenOptions with truncate(true) clears the file to zero length
        // while keeping the inode, so the kernel's O_APPEND fd stays valid.
        fs::OpenOptions::new()
            .write(true)
            .truncate(true)
            .open(&journal_path)
            .context("truncating journal")?;
    }
    let ctl_file = crate::ioctl::open(agfs).context("opening ctl for jump")?;
    crate::ioctl::jump(&ctl_file, 0, &[]).context("ioctl JUMP")?;
    Ok(())
}

pub fn run(force: bool) -> Result<()> {
    let agfs = crate::utils::session_dir()?;

    let journal = Journal::read(&agfs)?;
    let tree = journal.into_tree();
    if tree.is_empty() {
        println!("{}", "Nothing to discard.".yellow());
        return Ok(());
    }

    if !force {
        eprint!(
            "{} ",
            format!(
                "Discard {} staged change{}? [y/N]:",
                tree.len(),
                crate::utils::plural(tree.len())
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

    reset_staging(&agfs)?;

    println!("{}", "Staging discarded.".yellow().bold());

    Ok(())
}

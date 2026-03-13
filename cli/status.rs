// agfs CLI — status.rs
//
// `agfs status` — show staged changes (§3.10).

use crate::journal::{self, Change};
use anyhow::Result;
use colored::Colorize;

pub fn run() -> Result<()> {
    let agfs = crate::session_dir()?;

    let changes = journal::resolve(&agfs)?;

    if changes.is_empty() {
        println!("{}", "No changes staged.".yellow());
        return Ok(());
    }

    for change in &changes {
        match change {
            Change::Added { path, .. } => println!("  {} {}", path, "(added)".green()),
            Change::Modified { path, .. } => {
                println!("  {} {}", path, "(modified)".yellow())
            }
            Change::Deleted(p) => println!("  {} {}", p, "(deleted)".red()),
            Change::Renamed { from, to } => {
                println!("  {} → {} {}", from, to, "(renamed)".cyan())
            }
            Change::RenamedModified { from, to, .. } => {
                println!("  {} → {} {}", from, to, "(renamed + modified)".cyan())
            }
        }
    }

    let n = changes.len();
    println!(
        "\n{}",
        format!("{n} staged change{}", if n == 1 { "" } else { "s" }).bold()
    );
    Ok(())
}

// agfs CLI — status.rs
//
// `agfs status` — show staged changes (§3.10).
// `agfs status --at <name>` — show state at a snapshot (§3.11.4).

use crate::journal::{self, Change};
use anyhow::Result;
use colored::Colorize;

pub fn run(at: Option<&str>) -> Result<()> {
    let agfs = crate::utils::session_dir()?;

    let changes = match at {
        Some(name) => journal::resolve_at(&agfs, name)?,
        None => journal::resolve(&agfs)?,
    };

    if changes.is_empty() {
        println!("{}", "No changes staged.".yellow());
        return Ok(());
    }

    if let Some(name) = at {
        println!("{}", format!("State at snapshot \"{name}\":").dimmed());
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
        format!("{n} staged change{}", crate::utils::plural(n)).bold()
    );
    Ok(())
}

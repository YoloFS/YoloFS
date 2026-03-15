// agfs CLI — status.rs
//
// `agfs status` — show staged changes (§3.10).
// `agfs status --at <name>` — show state at a snapshot (§3.11.4).

use crate::journal::{self, Change, Section};
use anyhow::Result;
use colored::Colorize;

pub fn run(at: Option<&str>) -> Result<()> {
    let agfs = crate::utils::session_dir()?;

    if let Some(name) = at {
        let changes = journal::resolve_at(&agfs, name)?;
        if changes.is_empty() {
            println!("{}", "No changes staged.".yellow());
        } else {
            println!("{}", format!("State at snapshot \"{name}\":").dimmed());
            print_changes(&changes);
            print_total(changes.len());
        }
        return Ok(());
    }

    let sections = journal::resolve_sections(&agfs)?;
    let total: usize = sections.iter().map(|s| s.changes.len()).sum();

    if total == 0 {
        println!("{}", "No changes staged.".yellow());
        return Ok(());
    }

    let has_snapshots = sections.iter().any(|s| s.snapshot.is_some());
    if has_snapshots {
        print_sections(&sections);
    } else {
        print_changes(&sections[0].changes);
    }

    print_total(total);
    Ok(())
}

fn print_changes(changes: &[Change]) {
    for change in changes {
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
}

fn print_sections(sections: &[Section]) {
    for section in sections {
        match &section.snapshot {
            Some((id, name)) => {
                println!(
                    "{}",
                    format!("── snapshot \"{name}\" (id {id}) ──").cyan().bold()
                );
            }
            None => {
                println!("{}", "── (unsaved changes) ──".dimmed());
            }
        }
        if section.changes.is_empty() {
            println!("  {}", "(no changes)".dimmed());
        } else {
            print_changes(&section.changes);
        }
    }
}

fn print_total(n: usize) {
    println!(
        "\n{}",
        format!("{n} staged change{}", crate::utils::plural(n)).bold()
    );
}

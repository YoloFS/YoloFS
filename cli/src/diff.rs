// agfs CLI — diff.rs
//
// `agfs diff` — git-style unified diff of staged vs base (§3.6).

use crate::status;
use anyhow::Result;
use colored::Colorize;
use similar::TextDiff;
use std::fs;
use std::path::Path;

fn read_file_lossy(path: &Path) -> String {
    fs::read_to_string(path).unwrap_or_default()
}

fn print_unified_diff(old_text: &str, new_text: &str) {
    let diff = TextDiff::from_lines(old_text, new_text);

    for hunk in diff.unified_diff().context_radius(3).iter_hunks() {
        for change in hunk.iter_changes() {
            let line = change.to_string_lossy();
            match change.tag() {
                similar::ChangeTag::Delete => {
                    print!("{}", format!("-{line}").red());
                }
                similar::ChangeTag::Insert => {
                    print!("{}", format!("+{line}").green());
                }
                similar::ChangeTag::Equal => {
                    print!(" {line}");
                }
            }
            if change.missing_newline() {
                println!();
            }
        }
    }
}

/// Returns true if there were staged changes.
pub fn run() -> Result<bool> {
    let agfs = crate::session_dir()?;
    if !agfs.exists() {
        anyhow::bail!("no agfs session found (no .agfs/ directory)");
    }

    let staging_dir = agfs.join("staging");
    let base = Path::new("/");

    let changes = status::staging_walk(&agfs)?;

    if changes.is_empty() {
        println!("{}", "No changes staged.".yellow());
        return Ok(false);
    }

    for change in &changes {
        match change {
            status::Change::Added(p) => {
                let staging_file = staging_dir.join(p.trim_start_matches('/'));
                let new_text = read_file_lossy(&staging_file);
                println!("{} {}", p.bold(), "(added)".green());
                print_unified_diff("", &new_text);
            }
            status::Change::Modified(p) => {
                let base_file = base.join(p);
                let staging_file = staging_dir.join(p.trim_start_matches('/'));
                let old_text = read_file_lossy(&base_file);
                let new_text = read_file_lossy(&staging_file);
                if old_text != new_text {
                    println!("{} {}", p.bold(), "(modified)".yellow());
                    print_unified_diff(&old_text, &new_text);
                }
            }
            status::Change::Deleted(p) => {
                let base_file = base.join(p);
                let old_text = read_file_lossy(&base_file);
                println!("{} {}", p.bold(), "(deleted)".red());
                print_unified_diff(&old_text, "");
            }
            status::Change::Renamed { from, to } => {
                println!(
                    "{} → {} {}",
                    from.bold(),
                    to.bold(),
                    "(renamed)".cyan()
                );
            }
            status::Change::RenamedModified { from, to } => {
                let base_file = base.join(from);
                let staging_file = staging_dir.join(to.trim_start_matches('/'));
                let old_text = read_file_lossy(&base_file);
                let new_text = read_file_lossy(&staging_file);
                println!(
                    "{} → {} {}",
                    from.bold(),
                    to.bold(),
                    "(renamed + modified)".cyan()
                );
                if old_text != new_text {
                    print_unified_diff(&old_text, &new_text);
                }
            }
        }
        println!();
    }

    Ok(true)
}

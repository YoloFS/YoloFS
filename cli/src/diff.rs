// agfs CLI — diff.rs
//
// `agfs diff` — git-style unified diff of staged vs base (§3.6).

use crate::ctl;
use crate::status;
use anyhow::Result;
use colored::Colorize;
use similar::TextDiff;
use std::fs;
use std::path::Path;

fn read_file_lossy(path: &Path) -> String {
    fs::read_to_string(path).unwrap_or_default()
}

fn print_unified_diff(old_path: &str, new_path: &str, old_text: &str, new_text: &str) {
    let diff = TextDiff::from_lines(old_text, new_text);

    println!("{}", format!("--- a/{old_path}").bold());
    println!("{}", format!("+++ b/{new_path}").bold());

    for hunk in diff.unified_diff().context_radius(3).iter_hunks() {
        for change in hunk.iter_changes() {
            let sign = change.tag().to_string();
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

pub fn run() -> Result<()> {
    let agfs = ctl::agfs_dir()?;
    if !agfs.exists() {
        anyhow::bail!("no agfs session found (no .agfs/ directory)");
    }

    let staging_dir = agfs.join("staging");
    let base = Path::new("/");

    let changes = status::staging_walk(&agfs)?;

    if changes.is_empty() {
        println!("{}", "No changes staged.".yellow());
        return Ok(());
    }

    println!(
        "{}",
        "Changes detected in the following files:".green().bold()
    );
    println!();

    for change in &changes {
        match change {
            status::Change::Added(p) => {
                println!("  {} {}", p, "(added)".green());
            }
            status::Change::Modified(p) => {
                println!("  {} {}", p, "(modified)".yellow());
            }
            status::Change::Deleted(p) => {
                println!("  {} {}", p, "(deleted)".red());
            }
            status::Change::Renamed { from, to } => {
                println!("  {} → {} {}", from, to, "(renamed)".cyan());
            }
            status::Change::RenamedModified { from, to } => {
                println!(
                    "  {} → {} {}",
                    from,
                    to,
                    "(renamed + modified)".cyan()
                );
            }
        }
    }

    println!();

    for change in &changes {
        match change {
            status::Change::Added(p) => {
                let staging_file = staging_dir.join(p.trim_start_matches('/'));
                let new_text = read_file_lossy(&staging_file);
                println!("{}", format!("diff --agfs a/{p} b/{p}").bold());
                println!("{}", "new file".green());
                print_unified_diff("/dev/null", p, "", &new_text);
            }
            status::Change::Modified(p) => {
                let base_file = base.join(p);
                let staging_file = staging_dir.join(p.trim_start_matches('/'));
                let old_text = read_file_lossy(&base_file);
                let new_text = read_file_lossy(&staging_file);
                if old_text != new_text {
                    println!("{}", format!("diff --agfs a/{p} b/{p}").bold());
                    print_unified_diff(p, p, &old_text, &new_text);
                }
            }
            status::Change::Deleted(p) => {
                let base_file = base.join(p);
                let old_text = read_file_lossy(&base_file);
                println!(
                    "{}",
                    format!("diff --agfs a/{p} /dev/null").bold()
                );
                println!("{}", "deleted file".red());
                print_unified_diff(p, "/dev/null", &old_text, "");
            }
            status::Change::Renamed { from, to } => {
                println!(
                    "{}",
                    format!("diff --agfs a/{from} b/{to}").bold()
                );
                println!("{}", format!("rename from {from}").cyan());
                println!("{}", format!("rename to {to}").cyan());
            }
            status::Change::RenamedModified { from, to } => {
                let base_file = base.join(from);
                let staging_file = staging_dir.join(to.trim_start_matches('/'));
                let old_text = read_file_lossy(&base_file);
                let new_text = read_file_lossy(&staging_file);
                println!(
                    "{}",
                    format!("diff --agfs a/{from} b/{to}").bold()
                );
                println!("{}", format!("rename from {from}").cyan());
                println!("{}", format!("rename to {to}").cyan());
                if old_text != new_text {
                    print_unified_diff(from, to, &old_text, &new_text);
                }
            }
        }
    }

    Ok(())
}

// agfs CLI — diff.rs
//
// `agfs diff` — git-style unified diff of staged vs base (§3.10).
// `agfs diff --from <name>` — diff changes since a snapshot (§3.11.4).

use crate::journal::{self, Change};
use anyhow::Result;
use colored::Colorize;
use similar::TextDiff;
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

fn read_file_lossy(path: &Path) -> String {
    fs::read_to_string(path).unwrap_or_default()
}

fn read_blob(agfs: &Path, blob_id: u64) -> String {
    read_file_lossy(&journal::blob_path(agfs, blob_id))
}

fn read_base(base: &Path, rel_path: &str) -> String {
    read_file_lossy(&base.join(rel_path.trim_start_matches('/')))
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

/// Build a map of path → blob content for a set of resolved changes.
fn state_map(agfs: &Path, changes: &[Change]) -> BTreeMap<String, Option<String>> {
    let mut map = BTreeMap::new();
    let base = Path::new("/");
    for change in changes {
        match change {
            Change::Added { path, blob_id } | Change::Modified { path, blob_id } => {
                map.insert(path.clone(), Some(read_blob(agfs, *blob_id)));
            }
            Change::Deleted(path) => {
                map.insert(path.clone(), None);
            }
            Change::Renamed { from, to } => {
                map.insert(from.clone(), None);
                map.insert(to.clone(), Some(read_base(base, from)));
            }
            Change::RenamedModified { from, to, blob_id } => {
                map.insert(from.clone(), None);
                map.insert(to.clone(), Some(read_blob(agfs, *blob_id)));
            }
        }
    }
    map
}

/// Print staged diff. Returns true if there were staged changes.
pub fn run(from: Option<&str>) -> Result<bool> {
    let agfs = crate::utils::session_dir()?;
    if !agfs.exists() {
        anyhow::bail!("no agfs session found (no .agfs/ directory)");
    }

    if let Some(snap_name) = from {
        return run_from_snapshot(&agfs, snap_name);
    }

    let base = Path::new("/");
    let changes = journal::resolve(&agfs)?;

    if changes.is_empty() {
        println!("{}", "No changes staged.".yellow());
        return Ok(false);
    }

    for change in &changes {
        match change {
            Change::Added { path, blob_id } => {
                let new_text = read_blob(&agfs, *blob_id);
                println!("{} {}", path.bold(), "(added)".green());
                print_unified_diff("", &new_text);
            }
            Change::Modified { path, blob_id } => {
                let old_text = read_base(base, path);
                let new_text = read_blob(&agfs, *blob_id);
                if old_text != new_text {
                    println!("{} {}", path.bold(), "(modified)".yellow());
                    print_unified_diff(&old_text, &new_text);
                }
            }
            Change::Deleted(p) => {
                let old_text = read_base(base, p);
                println!("{} {}", p.bold(), "(deleted)".red());
                print_unified_diff(&old_text, "");
            }
            Change::Renamed { from, to } => {
                println!("{} → {} {}", from.bold(), to.bold(), "(renamed)".cyan());
            }
            Change::RenamedModified { from, to, blob_id } => {
                let old_text = read_base(base, from);
                let new_text = read_blob(&agfs, *blob_id);
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

/// Diff between snapshot state and current state.
fn run_from_snapshot(agfs: &Path, snap_name: &str) -> Result<bool> {
    let (snap_changes, current_changes) = journal::resolve_from(agfs, snap_name)?;

    let snap_state = state_map(agfs, &snap_changes);
    let current_state = state_map(agfs, &current_changes);

    // Collect all paths
    let mut all_paths: Vec<&String> = snap_state.keys().chain(current_state.keys()).collect();
    all_paths.sort();
    all_paths.dedup();

    let mut has_diff = false;

    for path in all_paths {
        let old = snap_state.get(path);
        let new = current_state.get(path);

        match (old, new) {
            (Some(Some(old_text)), Some(Some(new_text))) if old_text != new_text => {
                println!("{} {}", path.bold(), "(modified since snapshot)".yellow());
                print_unified_diff(old_text, new_text);
                println!();
                has_diff = true;
            }
            (None, Some(Some(new_text))) => {
                // Path exists in current but not in snapshot state
                println!("{} {}", path.bold(), "(added since snapshot)".green());
                print_unified_diff("", new_text);
                println!();
                has_diff = true;
            }
            (Some(Some(old_text)), None) => {
                // Path existed at snapshot but not in current
                println!("{} {}", path.bold(), "(removed since snapshot)".red());
                print_unified_diff(old_text, "");
                println!();
                has_diff = true;
            }
            (Some(Some(old_text)), Some(None)) => {
                // Was content at snapshot, now deleted
                println!("{} {}", path.bold(), "(deleted since snapshot)".red());
                print_unified_diff(old_text, "");
                println!();
                has_diff = true;
            }
            (Some(None), Some(Some(new_text))) => {
                // Was deleted at snapshot, now has content
                println!("{} {}", path.bold(), "(restored since snapshot)".green());
                print_unified_diff("", new_text);
                println!();
                has_diff = true;
            }
            _ => {}
        }
    }

    if !has_diff {
        println!(
            "{}",
            format!("No changes since snapshot \"{snap_name}\".").yellow()
        );
    }

    Ok(has_diff)
}

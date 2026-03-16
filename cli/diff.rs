// agfs CLI — diff.rs
//
// `agfs status` — one-line summary of staged changes.
// `agfs diff`   — git-style unified diff of staged vs base.
// `--at <name>` — show state at a snapshot.
// `--from <name>` — diff changes since a snapshot.

use crate::journal::{self, Change, Section};
use anyhow::Result;
use colored::Colorize;
use similar::TextDiff;
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

// ── File reading helpers ─────────────────────────────────────────────

fn read_file_lossy(path: &Path) -> String {
    fs::read_to_string(path).unwrap_or_default()
}

fn read_inode(agfs: &Path, ino: u64) -> String {
    read_file_lossy(&journal::inode_path(agfs, ino))
}

fn read_base(rel_path: &str) -> String {
    read_file_lossy(&crate::utils::to_base_path(rel_path))
}

// ── Unified diff printing ────────────────────────────────────────────

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

// ── Section display helpers ──────────────────────────────────────────

fn print_section_footer(section: &Section) {
    if let Some((id, name)) = &section.snapshot {
        println!(
            "  {} {}",
            format!("snapshot [{id}]").cyan().bold(),
            name.dimmed()
        );
    }
}

// ── Per-change printing (summary vs verbose) ─────────────────────────

fn print_change(agfs: &Path, change: &Change, verbose: bool) {
    let indent = if verbose { "" } else { "  " };
    match change {
        Change::Added { path, ino } => {
            println!("{indent}{} {}", path.bold(), "(added)".green());
            if verbose {
                print_unified_diff("", &read_inode(agfs, *ino));
            }
        }
        Change::Modified { path, ino } => {
            if verbose {
                let old_text = read_base(path);
                let new_text = read_inode(agfs, *ino);
                if old_text != new_text {
                    println!("{indent}{} {}", path.bold(), "(modified)".yellow());
                    print_unified_diff(&old_text, &new_text);
                }
            } else {
                println!("{indent}{} {}", path.bold(), "(modified)".yellow());
            }
        }
        Change::Deleted(p) => {
            println!("{indent}{} {}", p.bold(), "(deleted)".red());
            if verbose {
                print_unified_diff(&read_base(p), "");
            }
        }
        Change::Renamed { from, to } => {
            println!(
                "{indent}{} → {} {}",
                from.bold(),
                to.bold(),
                "(renamed)".cyan()
            );
        }
        Change::RenamedModified { from, to, ino } => {
            println!(
                "{indent}{} → {} {}",
                from.bold(),
                to.bold(),
                "(renamed + modified)".cyan()
            );
            if verbose {
                let old_text = read_base(from);
                let new_text = read_inode(agfs, *ino);
                if old_text != new_text {
                    print_unified_diff(&old_text, &new_text);
                }
            }
        }
    }
    if verbose {
        println!();
    }
}

fn print_changes(agfs: &Path, changes: &[Change], verbose: bool) {
    for change in changes {
        print_change(agfs, change, verbose);
    }
}

// ── Public entry points ──────────────────────────────────────────────

/// `agfs status` — summary view.
pub fn run_status(at: Option<&str>) -> Result<()> {
    let agfs = crate::utils::session_dir()?;

    if let Some(name) = at {
        let changes = journal::resolve_at(&agfs, name)?;
        if changes.is_empty() {
            println!("{}", "No changes staged.".yellow());
        } else {
            println!("{}", format!("State at snapshot \"{name}\":").dimmed());
            print_changes(&agfs, &changes, false);
            print_total(changes.len());
        }
        return Ok(());
    }

    run_sections(&agfs, false)?;
    Ok(())
}

/// `agfs diff` — verbose diff view. Returns true if there were changes.
pub fn run_diff(from: Option<&str>) -> Result<bool> {
    let agfs = crate::utils::session_dir()?;
    if !agfs.exists() {
        anyhow::bail!("no agfs session found (no .agfs/ directory)");
    }

    if let Some(snap_name) = from {
        return run_from_snapshot(&agfs, snap_name);
    }

    run_sections(&agfs, true)
}

// ── Shared implementation ────────────────────────────────────────────

fn run_sections(agfs: &Path, verbose: bool) -> Result<bool> {
    let sections = journal::resolve_sections(agfs)?;
    let total: usize = sections.iter().map(|s| s.changes.len()).sum();

    if total == 0 {
        println!("{}", "No changes staged.".yellow());
        return Ok(false);
    }

    let has_snapshots = sections.iter().any(|s| s.snapshot.is_some());

    for section in &sections {
        if has_snapshots {
            if section.snapshot.is_none() {
                println!("{}", "── (unsaved changes) ──".dimmed());
            }
            if section.changes.is_empty() {
                print_section_footer(section);
                continue;
            }
        }
        print_changes(agfs, &section.changes, verbose);
        if has_snapshots {
            print_section_footer(section);
        }
    }

    if !verbose {
        print_total(total);
    }

    Ok(true)
}

fn print_total(n: usize) {
    println!(
        "\n{}",
        format!("{n} staged change{}", crate::utils::plural(n)).bold()
    );
}

// ── Snapshot-to-current diff ─────────────────────────────────────────

/// Build a map of path → inode content for a set of resolved changes.
fn state_map(agfs: &Path, changes: &[Change]) -> BTreeMap<String, Option<String>> {
    let mut map = BTreeMap::new();
    for change in changes {
        match change {
            Change::Added { path, ino } | Change::Modified { path, ino } => {
                map.insert(path.clone(), Some(read_inode(agfs, *ino)));
            }
            Change::Deleted(path) => {
                map.insert(path.clone(), None);
            }
            Change::Renamed { from, to } => {
                map.insert(from.clone(), None);
                map.insert(to.clone(), Some(read_base(from)));
            }
            Change::RenamedModified { from, to, ino } => {
                map.insert(from.clone(), None);
                map.insert(to.clone(), Some(read_inode(agfs, *ino)));
            }
        }
    }
    map
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

        let (label, old_text, new_text) = match (old, new) {
            (Some(Some(old_text)), Some(Some(new_text))) if old_text != new_text => (
                "(modified since snapshot)".yellow(),
                old_text.as_str(),
                new_text.as_str(),
            ),
            (None, Some(Some(new_text))) => {
                ("(added since snapshot)".green(), "", new_text.as_str())
            }
            (Some(Some(old_text)), None) => {
                ("(removed since snapshot)".red(), old_text.as_str(), "")
            }
            (Some(Some(old_text)), Some(None)) => {
                ("(deleted since snapshot)".red(), old_text.as_str(), "")
            }
            (Some(None), Some(Some(new_text))) => {
                ("(restored since snapshot)".green(), "", new_text.as_str())
            }
            _ => continue,
        };

        println!("{} {}", path.bold(), label);
        print_unified_diff(old_text, new_text);
        println!();
        has_diff = true;
    }

    if !has_diff {
        println!(
            "{}",
            format!("No changes since snapshot \"{snap_name}\".").yellow()
        );
    }

    Ok(has_diff)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::journal::Change;
    use std::fs;
    use tempfile::TempDir;

    /// Create a temp dir that looks like an agfs session with staged inodes.
    /// Returns the TempDir (must be kept alive) and its path.
    fn make_agfs(inodes: &[(u64, &str)]) -> TempDir {
        let tmp = TempDir::new().unwrap();
        let inodes_dir = tmp.path().join("inodes");
        fs::create_dir_all(&inodes_dir).unwrap();
        for (ino, content) in inodes {
            fs::write(inodes_dir.join(ino.to_string()), content).unwrap();
        }
        tmp
    }

    #[test]
    fn state_map_empty_changes() {
        let tmp = make_agfs(&[]);
        let map = state_map(tmp.path(), &[]);
        assert!(map.is_empty());
    }

    #[test]
    fn state_map_added() {
        let tmp = make_agfs(&[(1, "hello\n")]);
        let changes = vec![Change::Added {
            path: "/src/main.rs".into(),
            ino: 1,
        }];
        let map = state_map(tmp.path(), &changes);
        assert_eq!(map.len(), 1);
        assert_eq!(map["/src/main.rs"], Some("hello\n".into()));
    }

    #[test]
    fn state_map_modified() {
        let tmp = make_agfs(&[(5, "new content")]);
        let changes = vec![Change::Modified {
            path: "/etc/config".into(),
            ino: 5,
        }];
        let map = state_map(tmp.path(), &changes);
        assert_eq!(map.len(), 1);
        assert_eq!(map["/etc/config"], Some("new content".into()));
    }

    #[test]
    fn state_map_deleted() {
        let tmp = make_agfs(&[]);
        let changes = vec![Change::Deleted("/old/file.txt".into())];
        let map = state_map(tmp.path(), &changes);
        assert_eq!(map.len(), 1);
        assert_eq!(map["/old/file.txt"], None);
    }

    #[test]
    fn state_map_renamed() {
        // Renamed reads base content via read_base(from). Since the `from` path
        // won't exist on the real filesystem, read_file_lossy returns "".
        let tmp = make_agfs(&[]);
        let changes = vec![Change::Renamed {
            from: "/nonexistent/old.rs".into(),
            to: "/nonexistent/new.rs".into(),
        }];
        let map = state_map(tmp.path(), &changes);
        assert_eq!(map.len(), 2);
        assert_eq!(map["/nonexistent/old.rs"], None);
        // read_base on a missing path returns ""
        assert_eq!(map["/nonexistent/new.rs"], Some(String::new()));
    }

    #[test]
    fn state_map_renamed_modified() {
        let tmp = make_agfs(&[(7, "modified content")]);
        let changes = vec![Change::RenamedModified {
            from: "/nonexistent/old.rs".into(),
            to: "/nonexistent/new.rs".into(),
            ino: 7,
        }];
        let map = state_map(tmp.path(), &changes);
        assert_eq!(map.len(), 2);
        assert_eq!(map["/nonexistent/old.rs"], None);
        assert_eq!(map["/nonexistent/new.rs"], Some("modified content".into()));
    }

    #[test]
    fn state_map_multiple_changes() {
        let tmp = make_agfs(&[(1, "aaa"), (2, "bbb")]);
        let changes = vec![
            Change::Added {
                path: "/a.txt".into(),
                ino: 1,
            },
            Change::Modified {
                path: "/b.txt".into(),
                ino: 2,
            },
            Change::Deleted("/c.txt".into()),
        ];
        let map = state_map(tmp.path(), &changes);
        assert_eq!(map.len(), 3);
        assert_eq!(map["/a.txt"], Some("aaa".into()));
        assert_eq!(map["/b.txt"], Some("bbb".into()));
        assert_eq!(map["/c.txt"], None);
    }
}

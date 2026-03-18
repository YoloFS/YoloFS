// agfs CLI — diff.rs
//
// `agfs status` — one-line summary of staged changes.
// `agfs diff`   — git-style unified diff of staged vs base.
// `--at <name>` — show state at a checkpoint.
// `--from <name>` — diff changes since a checkpoint.

use crate::journal;
use crate::resolve::{self, Change, Section};
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
    if let Some((id, name)) = &section.checkpoint {
        println!(
            "  {} {}",
            format!("checkpoint [{id}]").cyan().bold(),
            name.dimmed()
        );
    }
}

// ── Per-change printing (summary vs verbose) ─────────────────────────

fn print_change(agfs: &Path, change: &Change, verbose: bool) {
    let indent = if verbose { "" } else { "  " };
    match change {
        Change::Added { path, ino, .. } => {
            println!("{indent}{} {}", path.bold(), "(added)".green());
            if verbose {
                print_unified_diff("", &read_inode(agfs, *ino));
            }
        }
        Change::Modified { path, ino, .. } => {
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
        Change::Renamed { from, to, .. } => {
            println!(
                "{indent}{} → {} {}",
                from.bold(),
                to.bold(),
                "(renamed)".cyan()
            );
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
        let records = journal::read(&agfs)?.records;
        let changes = resolve::resolve_at(records, name)?;
        if changes.is_empty() {
            println!("{}", "No changes staged.".yellow());
        } else {
            println!("{}", format!("State at checkpoint \"{name}\":").dimmed());
            print_changes(&agfs, &changes, false);
            print_total(changes.len());
        }
        return Ok(());
    }

    run_sections(&agfs, false, None)?;
    Ok(())
}

/// `agfs diff` — verbose diff view. Returns true if there were changes.
pub fn run_diff(from: Option<&str>, path: Option<&str>) -> Result<bool> {
    let agfs = crate::utils::session_dir()?;
    if !agfs.exists() {
        anyhow::bail!("no agfs session found (no .agfs/ directory)");
    }

    let path = path.map(crate::utils::normalize_path);

    if let Some(chk_name) = from {
        return run_from_checkpoint(&agfs, chk_name, path.as_deref());
    }

    run_sections(&agfs, true, path.as_deref())
}

// ── Shared implementation ────────────────────────────────────────────

fn run_sections(agfs: &Path, verbose: bool, path: Option<&str>) -> Result<bool> {
    let records = journal::read(agfs)?.records;
    let sections = resolve::resolve_sections(records)?;

    let total: usize = match path {
        Some(target) => sections
            .iter()
            .flat_map(|s| &s.changes)
            .filter(|c| c.matches_path(target))
            .count(),
        None => sections.iter().map(|s| s.changes.len()).sum(),
    };

    if total == 0 {
        println!("{}", "No changes staged.".yellow());
        return Ok(false);
    }

    let has_checkpoints = sections.iter().any(|s| s.checkpoint.is_some());

    for section in &sections {
        let changes: Vec<&Change> = match path {
            Some(target) => section
                .changes
                .iter()
                .filter(|c| c.matches_path(target))
                .collect(),
            None => section.changes.iter().collect(),
        };

        if has_checkpoints {
            if changes.is_empty() && path.is_some() {
                continue;
            }
            if section.checkpoint.is_none() {
                println!("{}", "── (unsaved changes) ──".dimmed());
            }
            if changes.is_empty() {
                print_section_footer(section);
                continue;
            }
        }

        for change in &changes {
            print_change(agfs, change, verbose);
        }

        if has_checkpoints {
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

// ── Checkpoint-to-current diff ─────────────────────────────────────────

/// Build a map of path → inode content for a set of resolved changes.
fn state_map<'a>(agfs: &Path, changes: &'a [Change]) -> BTreeMap<&'a str, Option<String>> {
    let mut map = BTreeMap::new();
    for change in changes {
        match change {
            Change::Added { path, ino, .. } | Change::Modified { path, ino, .. } => {
                map.insert(path.as_str(), Some(read_inode(agfs, *ino)));
            }
            Change::Deleted(path) => {
                map.insert(path.as_str(), None);
            }
            Change::Renamed { from, to, .. } => {
                map.insert(from.as_str(), None);
                map.insert(to.as_str(), Some(read_base(from)));
            }
        }
    }
    map
}

/// Diff between checkpoint state and current state.
fn run_from_checkpoint(agfs: &Path, chk_name: &str, filter: Option<&str>) -> Result<bool> {
    let records = journal::read(agfs)?.records;
    let chk_idx = resolve::find_checkpoint_index(&records, chk_name)?;

    // Single-pass: snapshot at checkpoint, then continue to end.
    let mut resolver = resolve::Resolver::new();
    let mut records_iter = records.into_iter();
    for record in records_iter.by_ref().take(chk_idx + 1) {
        resolver.process(record);
    }
    let chk_changes = resolver.clone().into_changes();
    for record in records_iter {
        resolver.process(record);
    }
    let current_changes = resolver.into_changes();

    let chk_state = state_map(agfs, &chk_changes);
    let current_state = state_map(agfs, &current_changes);

    // Collect all paths
    let mut all_paths: Vec<&str> = chk_state.keys().chain(current_state.keys()).copied().collect();
    all_paths.sort();
    all_paths.dedup();

    if let Some(target) = filter {
        all_paths.retain(|p| *p == target);
    }

    let mut has_diff = false;

    for path in all_paths {
        let old = chk_state.get(path);
        let new = current_state.get(path);

        let (label, old_text, new_text) = match (old, new) {
            (Some(Some(old_text)), Some(Some(new_text))) if old_text != new_text => (
                "(modified since checkpoint)".yellow(),
                old_text.as_str(),
                new_text.as_str(),
            ),
            (None, Some(Some(new_text))) => {
                ("(added since checkpoint)".green(), "", new_text.as_str())
            }
            (Some(Some(old_text)), None) => {
                ("(removed since checkpoint)".red(), old_text.as_str(), "")
            }
            (Some(Some(old_text)), Some(None)) => {
                ("(deleted since checkpoint)".red(), old_text.as_str(), "")
            }
            (Some(None), Some(Some(new_text))) => {
                ("(restored since checkpoint)".green(), "", new_text.as_str())
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
            format!("No changes since checkpoint \"{chk_name}\".").yellow()
        );
    }

    Ok(has_diff)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resolve::Change;
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
            dtype: crate::journal::DType::File,
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
            dtype: crate::journal::DType::File,
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
            dtype: crate::journal::DType::File,
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
        let changes = vec![
            Change::Renamed {
                from: "/nonexistent/old.rs".into(),
                to: "/nonexistent/new.rs".into(),
                dtype: crate::journal::DType::File,
            },
            Change::Modified {
                path: "/nonexistent/new.rs".into(),
                ino: 7,
                dtype: crate::journal::DType::File,
            },
        ];
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
                dtype: crate::journal::DType::File,
            },
            Change::Modified {
                path: "/b.txt".into(),
                ino: 2,
                dtype: crate::journal::DType::File,
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

// agfs CLI — diff.rs
//
// `agfs status` — one-line summary of staged changes.
// `agfs diff`   — git-style unified diff of staged vs base.
// `--at <name>` — show state at a checkpoint.
// `--from <name>` — diff changes since a checkpoint.

use crate::journal;
use crate::resolve::{self, Change, Segment};
use anyhow::Result;
use colored::Colorize;
use similar::TextDiff;
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

// ── Segment display helpers ──────────────────────────────────────────

fn print_segment_footer(segment: &Segment) {
    if let Some((id, name)) = &segment.checkpoint {
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
            for change in &changes {
                print_change(&agfs, change, false);
            }
            print_total(changes.len());
        }
        return Ok(());
    }

    run_segments(&agfs, false, None)?;
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

    run_segments(&agfs, true, path.as_deref())
}

// ── Shared implementation ────────────────────────────────────────────

fn run_segments(agfs: &Path, verbose: bool, path: Option<&str>) -> Result<bool> {
    let records = journal::read(agfs)?.records;
    let segments = resolve::resolve_segments(records)?;

    let total: usize = match path {
        Some(target) => segments
            .iter()
            .flat_map(|s| &s.changes)
            .filter(|c| c.matches_path(target))
            .count(),
        None => segments.iter().map(|s| s.changes.len()).sum(),
    };

    if total == 0 {
        println!("{}", "No changes staged.".yellow());
        return Ok(false);
    }

    let has_checkpoints = segments.iter().any(|s| s.checkpoint.is_some());

    for segment in &segments {
        let changes: Vec<&Change> = match path {
            Some(target) => segment
                .changes
                .iter()
                .filter(|c| c.matches_path(target))
                .collect(),
            None => segment.changes.iter().collect(),
        };

        if has_checkpoints {
            if changes.is_empty() && path.is_some() {
                continue;
            }
            if segment.checkpoint.is_none() {
                println!("{}", "── (unsaved changes) ──".dimmed());
            }
            if changes.is_empty() {
                print_segment_footer(segment);
                continue;
            }
        }

        for change in &changes {
            print_change(agfs, change, verbose);
        }

        if has_checkpoints {
            print_segment_footer(segment);
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

/// Diff between checkpoint state and current state.
///
/// Segments are independently replayable, so the post-checkpoint records
/// form a self-contained delta.  We resolve only the tail and render it.
fn run_from_checkpoint(agfs: &Path, chk_name: &str, filter: Option<&str>) -> Result<bool> {
    let mut records = journal::read(agfs)?.records;
    let chk_idx = resolve::find_checkpoint_index(&records, chk_name)?;
    let tail = records.split_off(chk_idx + 1);
    drop(records);

    let changes = resolve::resolve(tail)?;
    let changes: Vec<&Change> = match filter {
        Some(target) => changes.iter().filter(|c| c.matches_path(target)).collect(),
        None => changes.iter().collect(),
    };

    if changes.is_empty() {
        println!(
            "{}",
            format!("No changes since checkpoint \"{chk_name}\".").yellow()
        );
        return Ok(false);
    }

    for change in &changes {
        let (label, old_text, new_text) = match change {
            Change::Added { path: _, ino, .. } => (
                "(added since checkpoint)".green(),
                String::new(),
                read_inode(agfs, *ino),
            ),
            Change::Modified { path, ino, .. } => (
                "(modified since checkpoint)".yellow(),
                read_base(path),
                read_inode(agfs, *ino),
            ),
            Change::Deleted(path) => (
                "(deleted since checkpoint)".red(),
                read_base(path),
                String::new(),
            ),
            Change::Renamed { from, to, .. } => (
                "(renamed since checkpoint)".yellow(),
                read_base(from),
                read_base(to),
            ),
        };

        let path = match change {
            Change::Added { path, .. }
            | Change::Modified { path, .. }
            | Change::Deleted(path) => path.as_str(),
            Change::Renamed { to, .. } => to.as_str(),
        };

        println!("{} {}", path.bold(), label);
        print_unified_diff(&old_text, &new_text);
        println!();
    }

    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resolve::Change;
    use std::collections::BTreeMap;
    use std::fs;
    use tempfile::TempDir;

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

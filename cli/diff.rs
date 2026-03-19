// agfs CLI — diff.rs
//
// `agfs status` — one-line summary of staged changes.
// `agfs diff`   — git-style unified diff of staged vs base.
// `--at <name>` — show state at a checkpoint (single segment).
// `--from <name>` — diff changes since a checkpoint.
// `--to <name>` — diff changes up to a checkpoint.
// `--from <name> --to <name>` — diff changes between two checkpoints.

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
    if let Some(c) = &segment.to {
        println!(
            "{} {}",
            format!("checkpoint [{}]", c.gen_id).cyan().bold(),
            c.name.dimmed()
        );
    }
}

// ── Per-change printing (summary vs verbose) ─────────────────────────

fn print_change(agfs: &Path, change: &Change, verbose: bool) {
    match change {
        Change::Added { path, ino, .. } => {
            println!("{} {}", path.bold(), "(added)".green());
            if verbose {
                print_unified_diff("", &read_inode(agfs, *ino));
            }
        }
        Change::Modified { path, ino, .. } => {
            if verbose {
                let old_text = read_base(path);
                let new_text = read_inode(agfs, *ino);
                if old_text != new_text {
                    println!("{} {}", path.bold(), "(modified)".yellow());
                    print_unified_diff(&old_text, &new_text);
                }
            } else {
                println!("{} {}", path.bold(), "(modified)".yellow());
            }
        }
        Change::Deleted(p) => {
            println!("{} {}", p.bold(), "(deleted)".red());
            if verbose {
                print_unified_diff(&read_base(p), "");
            }
        }
        Change::Renamed { from, to, .. } => {
            println!(
                "{} → {} {}",
                from.bold(),
                to.bold(),
                "(renamed)".cyan()
            );
        }
    }
}

// ── Public entry points ──────────────────────────────────────────────

/// `agfs status` — summary view.
pub fn run_status(at: Option<&str>, from: Option<&str>, to: Option<&str>) -> Result<()> {
    run(false, at, from, to, None)?;
    Ok(())
}

/// `agfs diff` — verbose diff view. Returns true if there were changes.
pub fn run_diff(
    at: Option<&str>,
    from: Option<&str>,
    to: Option<&str>,
    path: Option<&str>,
) -> Result<bool> {
    let path = path.map(crate::utils::normalize_path);
    run(true, at, from, to, path.as_deref())
}

// ── Core implementation ─────────────────────────────────────────────

fn run(
    verbose: bool,
    at: Option<&str>,
    from: Option<&str>,
    to: Option<&str>,
    path: Option<&str>,
) -> Result<bool> {
    let agfs = crate::utils::session_dir()?;
    let records = journal::read(&agfs)?.records;
    let records = resolve::extract_live(records);
    let records = resolve::slice_records(records, at, from, to)?;
    let segments = resolve::resolve_segments(records)?;

    let has_changes = segments
        .iter()
        .flat_map(|s| &s.changes)
        .any(|c| path.is_none_or(|t| c.matches_path(t)));

    if !has_changes {
        println!(
            "{}",
            format!("No changes{}.", range_label(at, from, to)).yellow()
        );
        return Ok(false);
    }

    let has_checkpoints = segments.iter().any(|s| s.to.is_some());
    let mut total = 0usize;

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
            if segment.to.is_none() {
                println!("{}", "── (unsaved changes) ──".dimmed());
            }
            if changes.is_empty() {
                print_segment_footer(segment);
                continue;
            }
        }

        for change in &changes {
            print_change(&agfs, change, verbose);
        }
        total += changes.len();

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

/// Human-readable label for the query range (empty string when no filter).
fn range_label(at: Option<&str>, from: Option<&str>, to: Option<&str>) -> String {
    match (at, from, to) {
        (Some(name), _, _) => format!(" at checkpoint \"{name}\""),
        (_, Some(f), Some(t)) => format!(" between \"{f}\" and \"{t}\""),
        (_, Some(f), None) => format!(" since checkpoint \"{f}\""),
        (_, None, Some(t)) => format!(" up to checkpoint \"{t}\""),
        _ => " staged".into(),
    }
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

    // -- range_label tests --

    #[test]
    fn range_label_at() {
        assert_eq!(range_label(Some("s1"), None, None), " at checkpoint \"s1\"");
    }

    #[test]
    fn range_label_from_to() {
        assert_eq!(
            range_label(None, Some("s1"), Some("s2")),
            " between \"s1\" and \"s2\""
        );
    }

    #[test]
    fn range_label_from_only() {
        assert_eq!(
            range_label(None, Some("s1"), None),
            " since checkpoint \"s1\""
        );
    }

    #[test]
    fn range_label_to_only() {
        assert_eq!(
            range_label(None, None, Some("s2")),
            " up to checkpoint \"s2\""
        );
    }

    #[test]
    fn range_label_none() {
        assert_eq!(range_label(None, None, None), " staged");
    }
}

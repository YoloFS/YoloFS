// yolo CLI — diff.rs
//
// `yolo status` — one-line summary of staged changes.
// `yolo diff`   — git-style unified diff of staged vs base.
// `--at <name>` — show state at a marker (single segment).
// `--from <name>` — diff changes since a marker.
// `--to <name>` — diff changes up to a marker.
// `--from <name> --to <name>` — diff changes between two markers.

use crate::journal::{DirTree, Journal, Marker, Target};
use anyhow::Result;
use colored::Colorize;
use similar::TextDiff;
use std::fs;
use std::path::Path;

// ── File reading helpers ─────────────────────────────────────────────

/// Read a file as text, or None if it's binary (contains null bytes).
fn read_file_text(path: &Path) -> Option<String> {
    let bytes = fs::read(path).ok()?;
    // Check first 8000 bytes for null bytes (same heuristic as git).
    let check_len = bytes.len().min(8000);
    if bytes[..check_len].contains(&0) {
        return None;
    }
    String::from_utf8(bytes).ok()
}

fn read_file_lossy(path: &Path) -> String {
    fs::read_to_string(path).unwrap_or_default()
}

fn read_inode(yolofs: &Path, ino: u32) -> String {
    read_file_lossy(&crate::utils::inode_path(yolofs, ino))
}

fn read_base(rel_path: &str) -> String {
    read_file_lossy(&crate::utils::to_base_path(rel_path))
}

fn is_binary_inode(yolofs: &Path, ino: u32) -> bool {
    read_file_text(&crate::utils::inode_path(yolofs, ino)).is_none()
}

fn is_binary_base(rel_path: &str) -> bool {
    read_file_text(&crate::utils::to_base_path(rel_path)).is_none()
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

fn print_segment_footer(closing: &Option<(u64, String)>) {
    if let Some((gen_id, label)) = closing {
        println!(
            "{} {}",
            format!("snapshot [{}]", gen_id).cyan().bold(),
            label.dimmed()
        );
    }
}

// ── Per-change printing (summary vs verbose) ─────────────────────────

fn print_change(
    yolofs: &Path,
    path: &str,
    target: &Target,
    verbose: bool,
    base_exists_cache: &mut std::collections::HashMap<String, bool>,
) {
    let base_exists = *base_exists_cache
        .entry(path.to_string())
        .or_insert_with(|| crate::utils::to_base_path(path).exists());
    match target {
        Target::StagedFile(ino) if !base_exists => {
            println!("{} {}", path.bold(), "(added)".green());
            if verbose {
                if is_binary_inode(yolofs, *ino) {
                    println!("  {}", "Binary file (not shown)".dimmed());
                } else {
                    print_unified_diff("", &read_inode(yolofs, *ino));
                }
            }
        }
        Target::StagedFile(ino) => {
            if verbose {
                let binary = is_binary_inode(yolofs, *ino) || is_binary_base(path);
                if binary {
                    println!("{} {}", path.bold(), "(modified)".yellow());
                    println!("  {}", "Binary files differ".dimmed());
                } else {
                    let old_text = read_base(path);
                    let new_text = read_inode(yolofs, *ino);
                    if old_text != new_text {
                        println!("{} {}", path.bold(), "(modified)".yellow());
                        print_unified_diff(&old_text, &new_text);
                    }
                }
            } else {
                println!("{} {}", path.bold(), "(modified)".yellow());
            }
        }
        Target::Tombstone if base_exists => {
            println!("{} {}", path.bold(), "(deleted)".red());
            if verbose {
                print_unified_diff(&read_base(path), "");
            }
        }
        Target::Tombstone => {} // spurious tombstone — staged-only file deleted; skip
        Target::BasePath(src) => {
            println!("{} → {} {}", src.bold(), path.bold(), "(renamed)".cyan());
        }
        Target::Passthrough => {} // passthrough — should not appear in iteration
    }
}

// ── Public entry points ──────────────────────────────────────────────

/// `yolo status` — summary view.
pub fn run_status(
    at: Option<&str>,
    from: Option<&str>,
    to: Option<&str>,
    quiet: bool,
) -> Result<()> {
    run(false, at, from, to, None, quiet)?;
    Ok(())
}

/// `yolo diff` — verbose diff view. Returns true if there were changes.
pub fn run_diff(
    at: Option<&str>,
    from: Option<&str>,
    to: Option<&str>,
    path: Option<&str>,
) -> Result<bool> {
    let path = path.map(crate::utils::normalize_path);
    run(true, at, from, to, path.as_deref(), false)
}

// ── Core implementation ─────────────────────────────────────────────

fn run(
    verbose: bool,
    at: Option<&str>,
    from: Option<&str>,
    to: Option<&str>,
    path: Option<&str>,
    quiet: bool,
) -> Result<bool> {
    let yolofs = crate::utils::session_dir()?;
    let journal = Journal::read(&yolofs)?;
    let num = journal.segments.len();
    let (start, end) = journal.markers.segment_range(at, from, to, num)?;

    // Precompute marker labels for live segments before consuming the journal.
    let labels: Vec<Option<(u64, String)>> = (start..end)
        .filter(|i| journal.is_alive(*i))
        .map(|i| {
            journal.markers.marker_at(i).map(|m| match m {
                Marker::Snapshot { gen_id, name } => (*gen_id, name.clone()),
                Marker::Travel {
                    gen_id, target_gen, ..
                } => (*gen_id, format!("traveled to [{target_gen}]")),
            })
        })
        .collect();
    let has_snapshots = labels.iter().any(|c| c.is_some());

    let mut total = 0usize;
    let mut base_exists_cache = std::collections::HashMap::new();

    for (seg, label) in journal.into_live_segments_range(start, end).zip(labels) {
        let tree = DirTree::build(std::iter::once(seg));

        // Count entries (filtered if a path is given).
        let count = match path {
            Some(target) => {
                let mut n = 0usize;
                tree.for_each(|p, t| {
                    if t.matches_path(p, target) {
                        n += 1;
                    }
                });
                n
            }
            None => tree.len(),
        };

        if has_snapshots {
            if count == 0 && path.is_some() {
                continue;
            }
            if count == 0 {
                print_segment_footer(&label);
                continue;
            }
        }

        tree.for_each(|p, target| {
            if path.is_none() || target.matches_path(p, path.unwrap()) {
                print_change(&yolofs, p, target, verbose, &mut base_exists_cache);
            }
        });
        total += count;

        if has_snapshots {
            print_segment_footer(&label);
        }
    }

    if total == 0 {
        if !quiet {
            println!(
                "{}",
                format!("No changes{}.", range_label(at, from, to)).yellow()
            );
        }
        return Ok(false);
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
        (Some(name), _, _) => format!(" at snapshot \"{name}\""),
        (_, Some(f), Some(t)) => format!(" between \"{f}\" and \"{t}\""),
        (_, Some(f), None) => format!(" since snapshot \"{f}\""),
        (_, None, Some(t)) => format!(" up to snapshot \"{t}\""),
        _ => " staged".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::journal::Target;
    use std::collections::BTreeMap;
    use std::fs;
    use tempfile::TempDir;

    fn state_map<'a>(
        yolofs: &Path,
        entries: &'a [(String, Target)],
    ) -> BTreeMap<&'a str, Option<String>> {
        let mut map = BTreeMap::new();
        for (path, target) in entries {
            match target {
                Target::StagedFile(ino) => {
                    map.insert(path.as_str(), Some(read_inode(yolofs, *ino)));
                }
                Target::Tombstone => {
                    map.insert(path.as_str(), None);
                }
                Target::BasePath(src) => {
                    map.insert(src.as_str(), None);
                    map.insert(path.as_str(), Some(read_base(src)));
                }
                Target::Passthrough => {} // passthrough
            }
        }
        map
    }

    /// Create a temp dir that looks like an yolofs session with staged inodes.
    /// Returns the TempDir (must be kept alive) and its path.
    fn make_yolofs(inodes: &[(u32, &str)]) -> TempDir {
        let tmp = TempDir::new().unwrap();
        let inodes_dir = tmp.path().join("inodes");
        fs::create_dir_all(&inodes_dir).unwrap();
        for (ino, content) in inodes {
            let path = crate::utils::inode_path(tmp.path(), *ino);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, content).unwrap();
        }
        tmp
    }

    #[test]
    fn state_map_empty_changes() {
        let tmp = make_yolofs(&[]);
        let map = state_map(tmp.path(), &[]);
        assert!(map.is_empty());
    }

    #[test]
    fn state_map_added() {
        let tmp = make_yolofs(&[(1, "hello\n")]);
        let dentries = vec![("/src/main.rs".into(), Target::StagedFile(1))];
        let map = state_map(tmp.path(), &dentries);
        assert_eq!(map.len(), 1);
        assert_eq!(map["/src/main.rs"], Some("hello\n".into()));
    }

    #[test]
    fn state_map_modified() {
        let tmp = make_yolofs(&[(5, "new content")]);
        let dentries = vec![("/etc/config".into(), Target::StagedFile(5))];
        let map = state_map(tmp.path(), &dentries);
        assert_eq!(map.len(), 1);
        assert_eq!(map["/etc/config"], Some("new content".into()));
    }

    #[test]
    fn state_map_deleted() {
        let tmp = make_yolofs(&[]);
        let dentries = vec![("/old/file.txt".into(), Target::Tombstone)];
        let map = state_map(tmp.path(), &dentries);
        assert_eq!(map.len(), 1);
        assert_eq!(map["/old/file.txt"], None);
    }

    #[test]
    fn state_map_renamed() {
        // Renamed reads base content via read_base(from). Since the `from` path
        // won't exist on the real filesystem, read_file_lossy returns "".
        let tmp = make_yolofs(&[]);
        let entries = vec![(
            "/nonexistent/new.rs".into(),
            Target::BasePath("/nonexistent/old.rs".into()),
        )];
        let map = state_map(tmp.path(), &entries);
        assert_eq!(map.len(), 2);
        assert_eq!(map["/nonexistent/old.rs"], None);
        // read_base on a missing path returns ""
        assert_eq!(map["/nonexistent/new.rs"], Some(String::new()));
    }

    #[test]
    fn state_map_renamed_modified() {
        let tmp = make_yolofs(&[(7, "modified content")]);
        let entries = vec![
            (
                "/nonexistent/new.rs".into(),
                Target::BasePath("/nonexistent/old.rs".into()),
            ),
            ("/nonexistent/new.rs".into(), Target::StagedFile(7)),
        ];
        let map = state_map(tmp.path(), &entries);
        assert_eq!(map.len(), 2);
        assert_eq!(map["/nonexistent/old.rs"], None);
        assert_eq!(map["/nonexistent/new.rs"], Some("modified content".into()));
    }

    #[test]
    fn state_map_multiple_changes() {
        let tmp = make_yolofs(&[(1, "aaa"), (2, "bbb")]);
        let dentries = vec![
            ("/a.txt".into(), Target::StagedFile(1)),
            ("/b.txt".into(), Target::StagedFile(2)),
            ("/c.txt".into(), Target::Tombstone),
        ];
        let map = state_map(tmp.path(), &dentries);
        assert_eq!(map.len(), 3);
        assert_eq!(map["/a.txt"], Some("aaa".into()));
        assert_eq!(map["/b.txt"], Some("bbb".into()));
        assert_eq!(map["/c.txt"], None);
    }

    // -- range_label tests --

    #[test]
    fn range_label_at() {
        assert_eq!(range_label(Some("s1"), None, None), " at snapshot \"s1\"");
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
            " since snapshot \"s1\""
        );
    }

    #[test]
    fn range_label_to_only() {
        assert_eq!(
            range_label(None, None, Some("s2")),
            " up to snapshot \"s2\""
        );
    }

    #[test]
    fn range_label_none() {
        assert_eq!(range_label(None, None, None), " staged");
    }
}

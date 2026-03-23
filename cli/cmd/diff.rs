// agfs CLI — diff.rs
//
// `agfs status` — one-line summary of staged changes.
// `agfs diff`   — git-style unified diff of staged vs base.
// `--at <name>` — show state at a checkpoint (single segment).
// `--from <name>` — diff changes since a checkpoint.
// `--to <name>` — diff changes up to a checkpoint.
// `--from <name> --to <name>` — diff changes between two checkpoints.

use crate::journal::{DirTree, Dstate, Journal};
use anyhow::Result;
use colored::Colorize;
use similar::TextDiff;
use std::fs;
use std::path::Path;

// ── File reading helpers ─────────────────────────────────────────────

fn read_file_lossy(path: &Path) -> String {
    fs::read_to_string(path).unwrap_or_default()
}

fn read_inode(agfs: &Path, ino: u32) -> String {
    read_file_lossy(&crate::utils::inode_path(agfs, ino))
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

fn print_segment_footer(closing: &Option<(u64, String)>) {
    if let Some((gen_id, name)) = closing {
        println!(
            "{} {}",
            format!("checkpoint [{}]", gen_id).cyan().bold(),
            name.dimmed()
        );
    }
}

// ── Per-change printing (summary vs verbose) ─────────────────────────

fn print_change(agfs: &Path, path: &str, dstate: &Dstate, verbose: bool) {
    match dstate {
        Dstate::StagedInode {
            ino,
            in_base: false,
            ..
        } => {
            println!("{} {}", path.bold(), "(added)".green());
            if verbose {
                print_unified_diff("", &read_inode(agfs, *ino));
            }
        }
        Dstate::StagedInode {
            ino, in_base: true, ..
        } => {
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
        Dstate::Tombstone { .. } => {
            println!("{} {}", path.bold(), "(deleted)".red());
            if verbose {
                print_unified_diff(&read_base(path), "");
            }
        }
        Dstate::Redirect { src, .. } => {
            println!("{} → {} {}", src.bold(), path.bold(), "(renamed)".cyan());
        }
        Dstate::Passthrough => {}
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
    let journal = Journal::read(&agfs)?;
    let num = journal.segments.len();
    let (start, end) = journal.markers.segment_range(at, from, to, num)?;

    // Precompute checkpoint labels for live segments before consuming the journal.
    let labels: Vec<Option<(u64, String)>> = (start..end)
        .filter(|i| journal.is_alive(*i))
        .map(|i| {
            journal
                .markers
                .checkpoint_at(i)
                .map(|(g, n)| (g, n.to_owned()))
        })
        .collect();
    let has_checkpoints = labels.iter().any(|c| c.is_some());

    let mut total = 0usize;

    for (seg, label) in journal.into_live_segments_range(start, end).zip(labels) {
        let tree = DirTree::build(std::iter::once(seg));

        // Count entries (filtered if a path is given).
        let count = match path {
            Some(target) => {
                let mut n = 0usize;
                tree.for_each(|p, d| {
                    if d.matches_path(p, target) {
                        n += 1;
                    }
                });
                n
            }
            None => tree.len(),
        };

        if has_checkpoints {
            if count == 0 && path.is_some() {
                continue;
            }
            if count == 0 {
                print_segment_footer(&label);
                continue;
            }
        }

        tree.for_each(|p, dstate| {
            if path.is_none() || dstate.matches_path(p, path.unwrap()) {
                print_change(&agfs, p, dstate, verbose);
            }
        });
        total += count;

        if has_checkpoints {
            print_segment_footer(&label);
        }
    }

    if total == 0 {
        println!(
            "{}",
            format!("No changes{}.", range_label(at, from, to)).yellow()
        );
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
    use crate::journal::Dstate;
    use std::collections::BTreeMap;
    use std::fs;
    use tempfile::TempDir;

    fn state_map<'a>(
        agfs: &Path,
        dstates: &'a [(String, Dstate)],
    ) -> BTreeMap<&'a str, Option<String>> {
        let mut map = BTreeMap::new();
        for (path, dstate) in dstates {
            match dstate {
                Dstate::StagedInode { ino, .. } => {
                    map.insert(path.as_str(), Some(read_inode(agfs, *ino)));
                }
                Dstate::Tombstone { .. } => {
                    map.insert(path.as_str(), None);
                }
                Dstate::Redirect { src, .. } => {
                    map.insert(src.as_str(), None);
                    map.insert(path.as_str(), Some(read_base(src)));
                }
                Dstate::Passthrough => {}
            }
        }
        map
    }

    /// Create a temp dir that looks like an agfs session with staged inodes.
    /// Returns the TempDir (must be kept alive) and its path.
    fn make_agfs(inodes: &[(u32, &str)]) -> TempDir {
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
        let dstates = vec![(
            "/src/main.rs".into(),
            Dstate::StagedInode {
                ino: 1,
                dtype: libc::DT_REG,
                in_base: false,
            },
        )];
        let map = state_map(tmp.path(), &dstates);
        assert_eq!(map.len(), 1);
        assert_eq!(map["/src/main.rs"], Some("hello\n".into()));
    }

    #[test]
    fn state_map_modified() {
        let tmp = make_agfs(&[(5, "new content")]);
        let dstates = vec![(
            "/etc/config".into(),
            Dstate::StagedInode {
                ino: 5,
                dtype: libc::DT_REG,
                in_base: true,
            },
        )];
        let map = state_map(tmp.path(), &dstates);
        assert_eq!(map.len(), 1);
        assert_eq!(map["/etc/config"], Some("new content".into()));
    }

    #[test]
    fn state_map_deleted() {
        let tmp = make_agfs(&[]);
        let dstates = vec![(
            "/old/file.txt".into(),
            Dstate::Tombstone {
                dtype: libc::DT_REG,
            },
        )];
        let map = state_map(tmp.path(), &dstates);
        assert_eq!(map.len(), 1);
        assert_eq!(map["/old/file.txt"], None);
    }

    #[test]
    fn state_map_renamed() {
        // Renamed reads base content via read_base(from). Since the `from` path
        // won't exist on the real filesystem, read_file_lossy returns "".
        let tmp = make_agfs(&[]);
        let dstates = vec![(
            "/nonexistent/new.rs".into(),
            Dstate::Redirect {
                src: "/nonexistent/old.rs".into(),
                dtype: libc::DT_REG,
                in_base: false,
            },
        )];
        let map = state_map(tmp.path(), &dstates);
        assert_eq!(map.len(), 2);
        assert_eq!(map["/nonexistent/old.rs"], None);
        // read_base on a missing path returns ""
        assert_eq!(map["/nonexistent/new.rs"], Some(String::new()));
    }

    #[test]
    fn state_map_renamed_modified() {
        let tmp = make_agfs(&[(7, "modified content")]);
        let dstates = vec![
            (
                "/nonexistent/new.rs".into(),
                Dstate::Redirect {
                    src: "/nonexistent/old.rs".into(),
                    dtype: libc::DT_REG,
                    in_base: false,
                },
            ),
            (
                "/nonexistent/new.rs".into(),
                Dstate::StagedInode {
                    ino: 7,
                    dtype: libc::DT_REG,
                    in_base: true,
                },
            ),
        ];
        let map = state_map(tmp.path(), &dstates);
        assert_eq!(map.len(), 2);
        assert_eq!(map["/nonexistent/old.rs"], None);
        assert_eq!(map["/nonexistent/new.rs"], Some("modified content".into()));
    }

    #[test]
    fn state_map_multiple_changes() {
        let tmp = make_agfs(&[(1, "aaa"), (2, "bbb")]);
        let dstates = vec![
            (
                "/a.txt".into(),
                Dstate::StagedInode {
                    ino: 1,
                    dtype: libc::DT_REG,
                    in_base: false,
                },
            ),
            (
                "/b.txt".into(),
                Dstate::StagedInode {
                    ino: 2,
                    dtype: libc::DT_REG,
                    in_base: true,
                },
            ),
            (
                "/c.txt".into(),
                Dstate::Tombstone {
                    dtype: libc::DT_REG,
                },
            ),
        ];
        let map = state_map(tmp.path(), &dstates);
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

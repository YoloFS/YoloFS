// yolo CLI — diff.rs
//
// `yolo status` — one-line summary of staged changes.
// `yolo diff`   — git-style unified diff of staged vs base.
// `--at <name>` — show state at a marker (single segment).
// `--from <name>` — diff changes since a marker.
// `--to <name>` — diff changes up to a marker.
// `--from <name> --to <name>` — diff changes between two markers.

use crate::changeset::Changeset;
use crate::journal::{DirTree, Journal, Note, Target};
use anyhow::Result;
use colored::Colorize;
use similar::TextDiff;
use std::fs;
use std::path::{Path, PathBuf};

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

// ── Per-change printing (summary vs verbose) ─────────────────────────

/// Display a journal path relative to the session root (git-style). Paths that
/// fall outside the root (e.g. `/etc/...`) are left absolute.
fn rel(path: &str, root: &Path) -> String {
    Path::new(path)
        .strip_prefix(root)
        .ok()
        .and_then(|p| p.to_str())
        .map(str::to_string)
        .unwrap_or_else(|| path.to_string())
}

/// A path's state in the "from" baseline (the previous snapshot, or the base
/// for `--full`): where its prior content lives, or `Absent`.
enum FromSide {
    Absent,
    Inode(u32),
    Base(String),
}

/// Resolve a path's state at the `from` baseline: its staged target there, or
/// — when not staged at `from` — the base file if it exists.
fn from_side(from: &DirTree, path: &str) -> FromSide {
    match from.get(path) {
        Some(Target::StagedFile(ino)) => FromSide::Inode(*ino),
        Some(Target::BasePath(src)) => FromSide::Base(src.clone()),
        Some(Target::Tombstone) => FromSide::Absent,
        Some(Target::Passthrough) | None => {
            // Not staged at `from`, so it's backed by the base — but a renamed
            // ancestor dir redirects it to its pre-rename base location, so
            // resolve through the redirects before checking the base.
            let resolved = from.resolve_base_path(path);
            if crate::utils::to_base_path(&resolved).exists() {
                FromSide::Base(resolved)
            } else {
                FromSide::Absent
            }
        }
    }
}

impl FromSide {
    fn exists(&self) -> bool {
        !matches!(self, FromSide::Absent)
    }
    fn content(&self, yolofs: &Path) -> String {
        match self {
            FromSide::Absent => String::new(),
            FromSide::Inode(ino) => read_inode(yolofs, *ino),
            FromSide::Base(p) => read_base(p),
        }
    }
    fn is_binary(&self, yolofs: &Path) -> bool {
        match self {
            FromSide::Absent => false,
            FromSide::Inode(ino) => is_binary_inode(yolofs, *ino),
            FromSide::Base(p) => is_binary_base(p),
        }
    }
}

// ── Classification (shared by status summary and diff bodies) ─────────

/// How a net change reads against the baseline. The single source of truth for
/// both `status` (one-line summary) and `diff` (unified body) — they must agree
/// on the verb and differ only in whether they also print content.
#[derive(Clone, Copy)]
enum ChangeKind {
    Added,
    Modified,
    Deleted,
    Renamed,
}

/// Classify a net target given whether its path existed at the baseline.
/// `None` means nothing to show: a delete of something already absent (a
/// create+delete that nets out), or a scaffold passthrough.
fn classify(target: &Target, present: bool) -> Option<ChangeKind> {
    match target {
        Target::StagedFile(_) if !present => Some(ChangeKind::Added),
        Target::StagedFile(_) => Some(ChangeKind::Modified),
        Target::Tombstone if present => Some(ChangeKind::Deleted),
        Target::Tombstone => None,
        Target::BasePath(_) => Some(ChangeKind::Renamed),
        Target::Passthrough => None,
    }
}

/// Print a change's one-line header. Renames read their source from the target.
fn print_header(kind: ChangeKind, shown: &str, target: &Target, root: &Path) {
    match kind {
        ChangeKind::Added => println!("{} {}", shown.bold(), "(added)".green()),
        ChangeKind::Modified => println!("{} {}", shown.bold(), "(modified)".yellow()),
        ChangeKind::Deleted => println!("{} {}", shown.bold(), "(deleted)".red()),
        ChangeKind::Renamed => {
            if let Target::BasePath(src) = target {
                println!(
                    "{} → {} {}",
                    rel(src, root).bold(),
                    shown.bold(),
                    "(renamed)".cyan()
                );
            }
        }
    }
}

/// Print one change as a git-style diff stanza (header + unified body),
/// classified against the `from` baseline tree (which also supplies the old
/// content). Returns whether it printed: a delete of something absent at
/// `from`, or a stage byte-identical to `from`, is a no-op and prints nothing.
fn print_diff(yolofs: &Path, root: &Path, from: &DirTree, path: &str, target: &Target) -> bool {
    // `path` stays absolute for base/inode lookups; `shown` is the display form.
    let shown = rel(path, root);
    let prev = from_side(from, path);
    let Some(kind) = classify(target, prev.exists()) else {
        return false;
    };
    match kind {
        ChangeKind::Added => {
            let ino = target.ino().expect("added ⇒ staged file");
            print_header(kind, &shown, target, root);
            if is_binary_inode(yolofs, ino) {
                println!("  {}", "Binary file (not shown)".dimmed());
            } else {
                print_unified_diff("", &read_inode(yolofs, ino));
            }
        }
        ChangeKind::Modified => {
            let ino = target.ino().expect("modified ⇒ staged file");
            if is_binary_inode(yolofs, ino) || prev.is_binary(yolofs) {
                print_header(kind, &shown, target, root);
                println!("  {}", "Binary files differ".dimmed());
            } else {
                let old_text = prev.content(yolofs);
                let new_text = read_inode(yolofs, ino);
                if old_text == new_text {
                    return false; // identical to `from` — not a real change
                }
                print_header(kind, &shown, target, root);
                print_unified_diff(&old_text, &new_text);
            }
        }
        ChangeKind::Deleted => {
            print_header(kind, &shown, target, root);
            print_unified_diff(&prev.content(yolofs), "");
        }
        ChangeKind::Renamed => print_header(kind, &shown, target, root),
    }
    true
}

// ── View: rendering the changeset ────────────────────────────────────

/// `status` view: one classified line per change, using the changeset's
/// `prev_present` map as the baseline (no tree rebuild, no base stat — the
/// O(segment) vs-previous-snapshot path). Returns how many were shown (no-op
/// deletes don't count).
fn render_summary(changeset: &Changeset, root: &Path) -> usize {
    let mut shown = 0;
    for (path, target) in &changeset.changes {
        let Some(kind) = classify(target, changeset.present_before(path)) else {
            continue;
        };
        print_header(kind, &rel(path, root), target, root);
        shown += 1;
    }
    shown
}

/// `diff` view: each change as a unified-diff stanza against the `from`
/// baseline tree. Returns how many were shown.
fn render(changeset: &Changeset, yolofs: &Path, root: &Path, from: &DirTree) -> usize {
    let mut shown = 0;
    for (path, target) in &changeset.changes {
        if print_diff(yolofs, root, from, path, target) {
            shown += 1;
        }
    }
    shown
}

/// Resolve which segments to show. Defaults to the latest batch of changes;
/// `--full` or an explicit `--at/--from/--to` range widens it.
fn resolve_range(
    journal: &Journal,
    at: Option<&str>,
    from: Option<&str>,
    to: Option<&str>,
    full: bool,
) -> Result<(usize, usize)> {
    let num = journal.segments.len();
    if at.is_some() || from.is_some() || to.is_some() {
        journal.markers.segment_range(at, from, to, num)
    } else if full {
        Ok((0, num))
    } else {
        let (start, end, _) = journal.latest_range();
        Ok((start, end))
    }
}

/// The default view (no `--full`, no explicit range) — where we always offer
/// the `--full` hint.
fn is_default_view(at: Option<&str>, from: Option<&str>, to: Option<&str>, full: bool) -> bool {
    at.is_none() && from.is_none() && to.is_none() && !full
}

// ── Public entry points ──────────────────────────────────────────────

/// Open the session: returns its `.yolofs` dir, the session root (its parent —
/// what paths display relative to), and the parsed journal.
fn open_session() -> Result<(PathBuf, PathBuf, Journal)> {
    let yolofs = crate::utils::session_dir()?;
    let root = yolofs.parent().unwrap_or(yolofs.as_path()).to_path_buf();
    let journal = Journal::read(&yolofs)?;
    Ok((yolofs, root, journal))
}

/// `yolo status` — summary of staged changes plus observed-access notes.
pub fn run_status(at: Option<&str>, from: Option<&str>, to: Option<&str>, full: bool) -> Result<()> {
    let (_yolofs, root, journal) = open_session()?;
    let (start, end) = resolve_range(&journal, at, from, to, full)?;
    // Classified vs the previous snapshot from the range's own records — no
    // previous-tree rebuild (O(segment), not O(journal)).
    let changeset = Changeset::collect(journal, start, end, None);
    let total = render_summary(&changeset, &root);

    if total == 0 {
        println!("{}", format!("No changes{}.", range_label(at, from, to)).yellow());
    } else {
        print_total(total);
    }
    if !changeset.notes.is_empty() {
        print_notes(&changeset.notes, &root);
    }
    // In the default view, always point at `--full` (when something is staged).
    if total > 0 && is_default_view(at, from, to, full) {
        print_full_hint(false);
    }
    Ok(())
}

/// `yolo diff` — verbose unified diff of staged vs base. Returns whether there
/// were any changes.
pub fn run_diff(
    at: Option<&str>,
    from: Option<&str>,
    to: Option<&str>,
    path: Option<&str>,
    full: bool,
) -> Result<bool> {
    let (yolofs, root, journal) = open_session()?;
    let (start, end) = resolve_range(&journal, at, from, to, full)?;
    let from_state = Journal::read(&yolofs)?.into_tree_at(start as u64);
    let path = path.map(crate::utils::normalize_path);
    let changeset = Changeset::collect(journal, start, end, path.as_deref());
    let total = render(&changeset, &yolofs, &root, &from_state);

    if total == 0 {
        println!("{}", format!("No changes{}.", range_label(at, from, to)).yellow());
    } else if is_default_view(at, from, to, full) {
        print_full_hint(true);
    }
    Ok(total > 0)
}

/// `yolo -- <cmd>` review: show what the just-run command changed, then a
/// summary line carrying the new snapshot's id — the handle for `yolo travel`.
pub fn run_after_exec(snapshot: Option<u64>) -> Result<()> {
    let (_yolofs, root, journal) = open_session()?;
    let (start, end) = resolve_range(&journal, None, None, None, false)?;
    let changeset = Changeset::collect(journal, start, end, None);
    let total = render_summary(&changeset, &root);

    if !changeset.notes.is_empty() {
        print_notes(&changeset.notes, &root);
    }
    match snapshot {
        // A subtle one-line footer: the new snapshot id is the handle for
        // `yolo travel <id>` to return here later.
        Some(gen_id) => println!(
            "{} {}",
            "yolo:".cyan(),
            format!(
                "snapshot [{gen_id}] · {total} staged change{} · yolo travel {gen_id} to return",
                crate::utils::plural(total)
            )
            .dimmed()
        ),
        // No snapshot (nothing staged, or auto-snapshot off): show the count if
        // any, else a quiet "(no changes)" — unless notes already said why.
        None if total > 0 => print_total(total),
        None if changeset.notes.is_empty() => println!("{}", "(no changes)".dimmed()),
        None => {}
    }
    Ok(())
}

/// Print observational notes (A/B) under `status`. These are denied or
/// ask-resolved accesses recorded in the visible range — not staged changes,
/// so they're listed separately and excluded from the staged-change count.
fn print_notes(notes: &[Note], root: &Path) {
    println!("\n{}", "Observed accesses (not staged):".bold());
    for note in notes {
        match note {
            Note::Block { path, op } => {
                println!(
                    "  {:8} {:5} {}",
                    "blocked".yellow(),
                    op.label(),
                    rel(path, root)
                );
            }
            Note::Ask {
                path,
                op,
                decision,
            } => {
                println!(
                    "  {:8} {:5} {} → {}",
                    "ask".yellow(),
                    op.label(),
                    rel(path, root),
                    decision
                );
            }
        }
    }
}

/// Point the user at `--full` for the complete history. The caller decides
/// when to show this (the default view, when something is staged).
fn print_full_hint(verbose: bool) {
    let cmd = if verbose { "diff" } else { "status" };
    println!(
        "{}",
        format!("(latest snapshot — run `yolo {cmd} --full` for all staged changes)").dimmed()
    );
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

    // ── classify (label rule shared by status + diff) ────────────────

    #[test]
    fn classify_all_combinations() {
        use ChangeKind::*;
        // A staged file is added when absent before, modified when present.
        assert!(matches!(classify(&Target::StagedFile(1), false), Some(Added)));
        assert!(matches!(classify(&Target::StagedFile(1), true), Some(Modified)));
        // A tombstone is a real delete only if the path existed before.
        assert!(matches!(classify(&Target::Tombstone, true), Some(Deleted)));
        assert!(classify(&Target::Tombstone, false).is_none());
        // A rename always reads as renamed, regardless of prior presence.
        assert!(matches!(
            classify(&Target::BasePath("/x".into()), true),
            Some(Renamed)
        ));
        assert!(matches!(
            classify(&Target::BasePath("/x".into()), false),
            Some(Renamed)
        ));
        // Scaffolds never show.
        assert!(classify(&Target::Passthrough, true).is_none());
    }

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

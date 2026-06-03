// yolo CLI — diff.rs
//
// `yolo status` — one-line summary of staged changes.
// `yolo diff`   — git-style unified diff of staged vs base.
// `--at <name>` — show state at a marker (single segment).
// `--from <name>` — diff changes since a marker.
// `--to <name>` — diff changes up to a marker.
// `--from <name> --to <name>` — diff changes between two markers.

use crate::changeset::{Change, Changeset};
use crate::journal::{Journal, Note, Target};
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

fn is_binary_inode(yolofs: &Path, ino: u32) -> bool {
    read_file_text(&crate::utils::inode_path(yolofs, ino)).is_none()
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

/// Print one change as a git-style diff stanza (header + unified body). The old
/// content comes from the change's pre-image (the previous-snapshot version);
/// the new content from the staged inode. Returns whether it printed — nothing
/// to show, or a stage byte-identical to its pre-image, prints nothing.
fn print_diff(yolofs: &Path, root: &Path, change: &Change) -> bool {
    let Change {
        path,
        target,
        preimage,
    } = change;
    let shown = rel(path, root);
    let Some(kind) = classify(target, preimage.is_some()) else {
        return false;
    };
    let pre = preimage.as_deref().map(Path::new);
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
            if is_binary_inode(yolofs, ino) || pre.is_some_and(|p| read_file_text(p).is_none()) {
                print_header(kind, &shown, target, root);
                println!("  {}", "Binary files differ".dimmed());
            } else {
                let old_text = pre.map(read_file_lossy).unwrap_or_default();
                let new_text = read_inode(yolofs, ino);
                if old_text == new_text {
                    return false; // identical to the pre-image — not a real change
                }
                print_header(kind, &shown, target, root);
                print_unified_diff(&old_text, &new_text);
            }
        }
        ChangeKind::Deleted => {
            print_header(kind, &shown, target, root);
            print_unified_diff(&pre.map(read_file_lossy).unwrap_or_default(), "");
        }
        ChangeKind::Renamed => print_header(kind, &shown, target, root),
    }
    true
}

// ── View: rendering the changeset ────────────────────────────────────

/// `status` view: one classified line per change, presence taken from the
/// change's pre-image (no tree rebuild, no base stat — the O(segment) path).
/// Returns how many were shown (no-op deletes don't count).
fn render_summary(changeset: &Changeset, root: &Path) -> usize {
    let mut shown = 0;
    for change in &changeset.changes {
        let Some(kind) = classify(&change.target, change.preimage.is_some()) else {
            continue;
        };
        print_header(kind, &rel(&change.path, root), &change.target, root);
        shown += 1;
    }
    shown
}

/// `diff` view: each change as a unified-diff stanza, old content from its
/// pre-image. Returns how many were shown.
fn render(changeset: &Changeset, yolofs: &Path, root: &Path) -> usize {
    let mut shown = 0;
    for change in &changeset.changes {
        if print_diff(yolofs, root, change) {
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
pub fn run_status(
    at: Option<&str>,
    from: Option<&str>,
    to: Option<&str>,
    full: bool,
) -> Result<()> {
    let (_yolofs, root, journal) = open_session()?;
    let (start, end) = resolve_range(&journal, at, from, to, full)?;
    // Classified vs the previous snapshot from the range's own records — no
    // previous-tree rebuild (O(segment), not O(journal)).
    let changeset = Changeset::collect(journal, start, end, None);
    let total = render_summary(&changeset, &root);

    if total == 0 {
        println!(
            "{}",
            format!("No changes{}.", range_label(at, from, to)).yellow()
        );
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
    let path = path.map(crate::utils::normalize_path);
    let changeset = Changeset::collect(journal, start, end, path.as_deref());
    let total = render(&changeset, &yolofs, &root);

    if total == 0 {
        println!(
            "{}",
            format!("No changes{}.", range_label(at, from, to)).yellow()
        );
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
            Note::Ask { path, op, decision } => {
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

    // ── classify (label rule shared by status + diff) ────────────────

    #[test]
    fn classify_all_combinations() {
        use ChangeKind::*;
        // A staged file is added when absent before, modified when present.
        assert!(matches!(
            classify(&Target::StagedFile(1), false),
            Some(Added)
        ));
        assert!(matches!(
            classify(&Target::StagedFile(1), true),
            Some(Modified)
        ));
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

// yolo CLI — review.rs
//
// Reviewing staged changes: `yolo review` (a summary, or a git-style diff with
// `--diff`) and the post-`yolo run -- <cmd>` review — both render the same
// `Changeset` model over a snapshot range.
//
// `review` takes an optional `[<id>[..<id>]]` spec selecting which snapshots to
// show (default: the latest, vs prev) and `--each` to expand a range into one
// stanza per consecutive snapshot. See the id/range grammar below.

use crate::changeset::{Change, Changeset};
use crate::journal::{Backing, Journal, Marker, Note};
use crate::report;
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
/// both the summary line and the `--diff` body — they must agree on the verb and
/// differ only in whether they also print content.
#[derive(Clone, Copy)]
enum ChangeKind {
    Added,
    Modified,
    Deleted,
    Renamed,
}

/// Classify a node from its `old` (the range-start version) and `new` (net
/// state). `None` means nothing to show: a scaffold (`new = None`) or a no-op
/// (absent → absent, a create+delete that nets out). A missing `old` on a
/// rendered node is treated defensively as `None` (absent).
fn classify(old: Option<&Backing>, new: Option<&Backing>) -> Option<ChangeKind> {
    let present = matches!(old, Some(Backing::StagedFile(_) | Backing::BasePath(_)));
    match new {
        None => None, // scaffold
        Some(Backing::StagedFile(_)) if present => Some(ChangeKind::Modified),
        Some(Backing::StagedFile(_)) => Some(ChangeKind::Added),
        Some(Backing::BasePath(_)) => Some(ChangeKind::Renamed),
        Some(Backing::None) if present => Some(ChangeKind::Deleted),
        Some(Backing::None) => None, // no-op
    }
}

/// Read a target's content for the old/new side of a diff, or `None` if binary
/// (or, for the old side, absent). `None` reads as empty.
fn read_target_text(yolofs: &Path, target: &Backing) -> Option<String> {
    match target {
        Backing::StagedFile(ino) => read_file_text(&crate::utils::inode_path(yolofs, *ino)),
        Backing::BasePath(p) => read_file_text(Path::new(p)),
        Backing::None => Some(String::new()),
    }
}

/// Lossy content for a target (empty for `None` / unreadable).
fn read_target_lossy(yolofs: &Path, target: &Backing) -> String {
    match target {
        Backing::StagedFile(ino) => read_inode(yolofs, *ino),
        Backing::BasePath(p) => read_file_lossy(Path::new(p)),
        Backing::None => String::new(),
    }
}

/// Print a change's one-line header. Renames read their source from `new`.
fn print_header(kind: ChangeKind, shown: &str, new: Option<&Backing>, root: &Path) {
    match kind {
        ChangeKind::Added => println!("{} {}", shown.bold(), "(added)".green()),
        ChangeKind::Modified => println!("{} {}", shown.bold(), "(modified)".yellow()),
        ChangeKind::Deleted => println!("{} {}", shown.bold(), "(deleted)".red()),
        ChangeKind::Renamed => {
            if let Some(Backing::BasePath(src)) = new {
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
/// content comes from `old` (the range-start version); the new content from
/// `new`'s staged inode. Returns whether it printed — nothing to show, or a
/// stage byte-identical to its old side, prints nothing.
fn print_diff(yolofs: &Path, root: &Path, change: &Change) -> bool {
    let Change { path, old, new } = change;
    let shown = rel(path, root);
    let Some(kind) = classify(old.as_ref(), new.as_ref()) else {
        return false;
    };
    let old_binary = old
        .as_ref()
        .is_some_and(|s| read_target_text(yolofs, s).is_none());
    match kind {
        ChangeKind::Added => {
            let ino = new
                .as_ref()
                .and_then(Backing::ino)
                .expect("added ⇒ staged file");
            print_header(kind, &shown, new.as_ref(), root);
            if is_binary_inode(yolofs, ino) {
                println!("  {}", "Binary file (not shown)".dimmed());
            } else {
                print_unified_diff("", &read_inode(yolofs, ino));
            }
        }
        ChangeKind::Modified => {
            let ino = new
                .as_ref()
                .and_then(Backing::ino)
                .expect("modified ⇒ staged file");
            if is_binary_inode(yolofs, ino) || old_binary {
                print_header(kind, &shown, new.as_ref(), root);
                println!("  {}", "Binary files differ".dimmed());
            } else {
                let old_text = old
                    .as_ref()
                    .map(|s| read_target_lossy(yolofs, s))
                    .unwrap_or_default();
                let new_text = read_inode(yolofs, ino);
                if old_text == new_text {
                    return false; // identical to the old side — not a real change
                }
                print_header(kind, &shown, new.as_ref(), root);
                print_unified_diff(&old_text, &new_text);
            }
        }
        ChangeKind::Deleted => {
            print_header(kind, &shown, new.as_ref(), root);
            let old_text = old
                .as_ref()
                .map(|s| read_target_lossy(yolofs, s))
                .unwrap_or_default();
            print_unified_diff(&old_text, "");
        }
        ChangeKind::Renamed => print_header(kind, &shown, new.as_ref(), root),
    }
    true
}

// ── View: rendering the changeset ────────────────────────────────────

/// The changes in a set that actually render, each with its kind. Drops what
/// shows nothing — a no-op delete (create+delete that nets out) or a passthrough
/// scaffold. The single definition of "what shows," shared by the summary, the
/// `--diff` body, and the `--each` emptiness check.
fn classified(changeset: &Changeset) -> Vec<(&Change, ChangeKind)> {
    changeset
        .changes
        .iter()
        .filter_map(|c| classify(c.old.as_ref(), c.new.as_ref()).map(|kind| (c, kind)))
        .collect()
}

/// Summary view: one classified line per change, presence taken from the
/// change's pre-image (no tree rebuild, no base stat — the O(segment) path).
/// Returns how many were shown (no-op deletes don't count).
fn render_summary(changeset: &Changeset, root: &Path) -> usize {
    let items = classified(changeset);
    for (change, kind) in &items {
        print_header(*kind, &rel(&change.path, root), change.new.as_ref(), root);
    }
    items.len()
}

/// `--diff` view: each change as a unified-diff stanza, old content from its
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

// ── Snapshot-id / range grammar: `[<id>[..<id>]]` ────────────────────
//
// The positional selects which segments to classify, all relative to the
// range's start (the pre-image baseline):
//   (none)  → latest segment (vs prev); the whole session under `--each`
//   N       → snapshot N's own change, `prev(N)..N`
//   a..b    → state(a) → state(b); empty end means base (0) / tip
//   all     → the whole session, base..tip (everything vs base); same as `..`
// Ids are numbers (0 is the base/"(initial)" marker).

/// Translate a positional id/range spec into a segment range `[start, end)`.
/// This parses the generation ids out of the spec and hands `u64`s to
/// `MarkerIndex::segment_range` (`0` = base). Shared with `yolo audit`,
/// which takes the same `[<id>|a..b|all]` grammar.
pub(crate) fn parse_range(
    spec: Option<&str>,
    each: bool,
    journal: &Journal,
) -> Result<(usize, usize)> {
    let num = journal.segments.len();
    let Some(spec) = spec else {
        // No spec: the latest segment (vs prev), or the whole session under
        // `--each` so `yolo review --each` walks every snapshot.
        return Ok(if each {
            (0, num)
        } else {
            let (start, end, _) = journal.latest_range();
            (start, end)
        });
    };
    // `all` — the readable name for the whole session (`..`), everything vs base.
    if spec == "all" {
        return Ok((0, num));
    }
    // Endpoints are generation ids; an empty endpoint is an open end (base/tip).
    let endpoint = |t: &str| -> Result<Option<u64>> {
        (!t.is_empty())
            .then(|| crate::utils::parse_gen(t))
            .transpose()
    };
    match spec.split_once("..") {
        // `a..b`: a range (empty ends → base / tip).
        Some((a, b)) => {
            let from = endpoint(a)?;
            let to = endpoint(b)?;
            journal.markers.segment_range(None, from, to, num)
        }
        // Bare id N: that snapshot's own change (`prev(N)..N`).
        None => journal
            .markers
            .segment_range(endpoint(spec)?, None, None, num),
    }
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

/// `yolo review [<id>[..<id>]] [--diff] [--each]` — review staged
/// changes over a range. The default is a one-line-per-change summary; `--diff`
/// renders the git-style unified body instead. Everything is classified vs the
/// range's start (the pre-image baseline — no previous-tree rebuild; O(segment),
/// not O(journal)).
pub fn run_review(range: Option<&str>, each: bool, diff: bool) -> Result<()> {
    let (yolofs, root, journal) = open_session()?;
    let (start, end) = parse_range(range, each, &journal)?;
    if each {
        render_each(&journal, &yolofs, &root, start, end, diff);
        return Ok(());
    }
    let changeset = Changeset::collect(&journal, start, end);
    let total = if diff {
        render(&changeset, &yolofs, &root)
    } else {
        render_summary(&changeset, &root)
    };

    if total == 0 {
        report::empty(format!("no changes{}", range_label(range)));
    } else if !diff {
        // The summary closes with a count; the diff body speaks for itself.
        print_total(total);
    }
    // Audit notes ride along the summary, not the diff body (which
    // shows staged content only).
    if !diff && !changeset.notes.is_empty() {
        print_notes(&changeset.notes, &root);
    }
    // In the default view, point at the vs-base range and the per-snapshot view.
    if total > 0 && range.is_none() {
        print_base_hint();
    }
    Ok(())
}

/// `--each`: one stanza per relevant journal segment in `[start, end)`, headed
/// by its sealing snapshot/travel marker or `working` for the unsealed tip.
/// Dead filesystem records stay filtered, while chronological C notes remain
/// visible. Empty/netted-out segments are skipped. Returns whether anything was
/// shown.
fn render_each(
    journal: &Journal,
    yolofs: &Path,
    root: &Path,
    start: usize,
    end: usize,
    verbose: bool,
) -> bool {
    let mut shown_any = false;
    for i in start..end {
        if journal.segments[i].records.is_empty() {
            continue;
        }
        let changeset = Changeset::collect(journal, i, i + 1);
        if classified(&changeset).is_empty() && changeset.notes.is_empty() {
            continue;
        }
        if shown_any {
            println!();
        }
        // Segment i is sealed by marker i+1. C notes can keep a dead,
        // travel-sealed segment visible, so label both marker kinds exactly.
        match journal.markers.get(i + 1) {
            Some(Marker::Snapshot { name }) => println!(
                "{} {}",
                format!("snapshot {}", i + 1).cyan().bold(),
                name.dimmed()
            ),
            Some(Marker::Travel { target_gen }) => println!(
                "{} → {}",
                format!("travel {}", i + 1).yellow().bold(),
                target_gen
            ),
            None => println!("{}", "working".bold()),
        }
        if verbose {
            render(&changeset, yolofs, root);
        } else {
            render_summary(&changeset, root);
        }
        if !verbose && !changeset.notes.is_empty() {
            print_notes(&changeset.notes, root);
        }
        shown_any = true;
    }
    if !shown_any {
        report::empty("no changes");
    }
    shown_any
}

/// `yolo run -- <cmd>` review: show what the just-run command changed, then a
/// summary line carrying the new snapshot's id — the handle for `yolo travel`.
pub fn run_after_exec(snapshot: Option<u64>) -> Result<()> {
    let (_yolofs, root, journal) = open_session()?;
    let num = journal.segments.len();
    // Show what THIS command did — not `latest_range`'s fallback to an older
    // batch when the command itself changed nothing. If it auto-snapshotted,
    // show the segment the new snapshot captured; otherwise the (possibly empty)
    // tail since the last snapshot — empty ⇒ "(no changes)", never an old batch.
    let (start, end) = match snapshot {
        Some(gen_id) => {
            let m = (gen_id as usize).min(num);
            (journal.markers.prev_snapshot_idx(m), m)
        }
        None => (journal.markers.last_snapshot_idx().unwrap_or(0), num),
    };
    let changeset = Changeset::collect(&journal, start, end);
    let total = render_summary(&changeset, &root);

    if !changeset.notes.is_empty() {
        print_notes(&changeset.notes, &root);
    }
    match snapshot {
        // A one-line status footer: count first, then the new snapshot id as
        // the handle for `yolo travel <id>` to return here later.
        Some(gen_id) => report::info(format!(
            "{total} staged change{} in snapshot {gen_id} · `yolo travel {gen_id}` to return",
            crate::utils::plural(total)
        )),
        // No snapshot (nothing staged, or auto-snapshot off): show the count if
        // any, else a quiet "(no changes)" — unless notes already said why.
        None if total > 0 => print_total(total),
        None if changeset.notes.is_empty() => report::empty("no changes"),
        None => {}
    }
    Ok(())
}

/// Print observational notes (G/C) under the review. These are gate results or
/// policy configurations recorded in the visible range — not staged changes, so
/// they're grouped under a `not staged:` header and excluded from the count. The
/// line shape mirrors a change (`path (kind)`), but dimmed so it reads as an
/// access, not a commit; the op (and ask decision) ride in the parenthetical.
fn print_notes(notes: &[Note], root: &Path) {
    println!("\n{}", "not staged:".dimmed());
    for note in notes {
        let (path, kind) = match note {
            Note::Gate { path, op, result } => (
                path,
                match result {
                    crate::journal::GateResult::DirectDeny => format!("denied {}", op.label()),
                    crate::journal::GateResult::AskAllow => format!("asked {} → yes", op.label()),
                    crate::journal::GateResult::AskDeny => format!("asked {} → no", op.label()),
                },
            ),
            Note::Configure { path, policy } => (path, format!("configured = {}", policy.label())),
        };
        println!(
            "{} {}",
            rel(path, root).dimmed(),
            format!("({kind})").yellow()
        );
    }
}

/// In the default view (latest snapshot, something staged), point at the
/// vs-base range. Shares its shape with `yolo audit`'s footer.
fn print_base_hint() {
    println!(
        "{}",
        "(latest snapshot · `yolo review all` for everything since base)".dimmed()
    );
}

fn print_total(n: usize) {
    println!(
        "{}",
        format!("{n} staged change{}", crate::utils::plural(n)).bold()
    );
}

/// Human-readable suffix for "(no changes…)" — names the queried range.
fn range_label(spec: Option<&str>) -> String {
    match spec {
        None => " staged".into(),
        Some(s) => format!(" in `{s}`"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::journal::Backing;

    // ── classify (label rule shared by status + diff) ────────────────

    #[test]
    fn classify_all_combinations() {
        use ChangeKind::*;
        let staged = Backing::StagedFile(1);
        let base = Backing::BasePath("/x".into());
        let absent = Backing::None;
        // A staged end is added when the old side is absent, modified when present.
        assert!(matches!(
            classify(Some(&absent), Some(&staged)),
            Some(Added)
        ));
        assert!(matches!(
            classify(Some(&base), Some(&staged)),
            Some(Modified)
        ));
        // An absent end is a real delete only if the old side was present.
        assert!(matches!(
            classify(Some(&base), Some(&absent)),
            Some(Deleted)
        ));
        assert!(classify(Some(&absent), Some(&absent)).is_none());
        // A base-path end always reads as renamed, regardless of the old side.
        assert!(matches!(
            classify(Some(&absent), Some(&base)),
            Some(Renamed)
        ));
        assert!(matches!(classify(Some(&base), Some(&base)), Some(Renamed)));
        // Scaffolds (end = None) never show.
        assert!(classify(Some(&base), None).is_none());
        assert!(classify(None, None).is_none());
    }

    // ── id / range grammar ───────────────────────────────────────────

    #[test]
    fn range_label_names_the_spec() {
        assert_eq!(range_label(None), " staged");
        assert_eq!(range_label(Some("3..5")), " in `3..5`");
    }
}

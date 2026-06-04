// yolo CLI — review.rs
//
// Reviewing staged changes: `yolo review` (a summary, or a git-style diff with
// `--diff`) and the post-`yolo -- <cmd>` review — both render the same
// `Changeset` model over a snapshot range.
//
// `review` takes an optional `[<id>[..<id>]]` spec selecting which snapshots to
// show (default: the latest, vs prev) and `--each` to expand a range into one
// stanza per consecutive snapshot. See the id/range grammar below.

use crate::changeset::{Change, Changeset};
use crate::journal::{Journal, Marker, Note, Target};
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

/// The changes in a set that actually render, each with its kind. Drops what
/// shows nothing — a no-op delete (create+delete that nets out) or a passthrough
/// scaffold. The single definition of "what shows," shared by the summary, the
/// `--diff` body, and the `--each` emptiness check.
fn classified(changeset: &Changeset) -> Vec<(&Change, ChangeKind)> {
    changeset
        .changes
        .iter()
        .filter_map(|c| classify(&c.target, c.preimage.is_some()).map(|kind| (c, kind)))
        .collect()
}

/// Summary view: one classified line per change, presence taken from the
/// change's pre-image (no tree rebuild, no base stat — the O(segment) path).
/// Returns how many were shown (no-op deletes don't count).
fn render_summary(changeset: &Changeset, root: &Path) -> usize {
    let items = classified(changeset);
    for (change, kind) in &items {
        print_header(*kind, &rel(&change.path, root), &change.target, root);
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
// Ids are numbers (0 is the base/"(initial)" marker). The optional `--diff`
// path filter is passed after `--`, so the positional is unambiguously a range.

/// Translate a positional id/range spec into a segment range `[start, end)`,
/// delegating the resolution to `MarkerIndex::segment_range`. Ids are numbers
/// only (`0` = base); names are rejected here so the positional can't be
/// mistaken for a path — the `--diff` path filter is passed after `--`. Shared
/// with `yolo journal`, which takes the same `[<id>|a..b|all]` grammar.
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
    // Endpoints must be numeric (empty = an open end).
    let numeric = |t: &str| t.is_empty() || t.bytes().all(|b| b.is_ascii_digit());
    match spec.split_once("..") {
        // `a..b`: a range (empty ends → base / tip).
        Some((a, b)) if numeric(a) && numeric(b) => {
            let from = (!a.is_empty()).then_some(a);
            let to = (!b.is_empty()).then_some(b);
            journal.markers.segment_range(None, from, to, num)
        }
        // Bare id N: that snapshot's own change (`prev(N)..N`).
        None if numeric(spec) => journal.markers.segment_range(Some(spec), None, None, num),
        _ => anyhow::bail!("`{spec}` is not a snapshot id or range (see `yolo timeline`)"),
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

/// `yolo review [<id>[..<id>]] [--diff] [--each] [-- <path>]` — review staged
/// changes over a range. The default is a one-line-per-change summary; `--diff`
/// renders the git-style unified body instead. Everything is classified vs the
/// range's start (the pre-image baseline — no previous-tree rebuild; O(segment),
/// not O(journal)).
pub fn run_review(range: Option<&str>, path: Option<&str>, each: bool, diff: bool) -> Result<()> {
    let (yolofs, root, journal) = open_session()?;
    let (start, end) = parse_range(range, each, &journal)?;
    let path = path.map(crate::utils::normalize_path);
    if each {
        render_each(&journal, &yolofs, &root, path.as_deref(), start, end, diff);
        return Ok(());
    }
    let changeset = Changeset::collect(&journal, start, end, path.as_deref());
    let total = if diff {
        render(&changeset, &yolofs, &root)
    } else {
        render_summary(&changeset, &root)
    };

    if total == 0 {
        println!("{}", format!("No changes{}.", range_label(range)).yellow());
    } else if !diff {
        // The summary closes with a count; the diff body speaks for itself.
        print_total(total);
    }
    // Observed-access notes ride along the summary, not the diff body (which
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

/// `--each`: one stanza per consecutive snapshot in `[start, end)`. Each live,
/// non-empty segment `i` is the change snapshot `i+1` captured (gen id == marker
/// index), so it's headed `snapshot <i+1>` — except the tip (work after the last
/// snapshot has no sealing marker, so no snapshot captures it), headed `working`.
/// Empty/netted-out segments are skipped. Returns whether anything was shown.
fn render_each(
    journal: &Journal,
    yolofs: &Path,
    root: &Path,
    path: Option<&str>,
    start: usize,
    end: usize,
    verbose: bool,
) -> bool {
    let mut shown_any = false;
    for i in start..end {
        if !journal.is_alive(i) || journal.segments[i].records.is_empty() {
            continue;
        }
        let changeset = Changeset::collect(journal, i, i + 1, path);
        if classified(&changeset).is_empty() && changeset.notes.is_empty() {
            continue;
        }
        if shown_any {
            println!();
        }
        // Segment i is sealed by marker i+1 (a snapshot, since travel-sealed
        // segments are dead and skipped above). The tip — uncommitted work past
        // the last snapshot — has no sealing marker, so it isn't a snapshot yet.
        // Head it like `timeline` does: `snapshot <id> <name>`.
        if i + 1 < journal.markers.len() {
            let name = match journal.markers.get(i + 1) {
                Some(Marker::Snapshot { name, .. }) => name.as_str(),
                _ => "",
            };
            println!(
                "{} {}",
                format!("snapshot {}", i + 1).cyan().bold(),
                name.dimmed()
            );
        } else {
            println!("{}", "working".bold());
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
        println!("{}", "No changes.".yellow());
    }
    shown_any
}

/// `yolo -- <cmd>` review: show what the just-run command changed, then a
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
    let changeset = Changeset::collect(&journal, start, end, None);
    let total = render_summary(&changeset, &root);

    if !changeset.notes.is_empty() {
        print_notes(&changeset.notes, &root);
    }
    match snapshot {
        // A subtle one-line footer: count first, then the new snapshot id as the
        // handle for `yolo travel <id>` to return here later.
        Some(gen_id) => println!(
            "{} {}",
            "yolo:".cyan(),
            format!(
                "{total} staged change{} in snapshot {gen_id} · `yolo travel {gen_id}` to return",
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

/// Print observational notes (A/B) under the review. These are denied or
/// ask-resolved accesses recorded in the visible range — not staged changes, so
/// they're grouped under a `not staged:` header and excluded from the count. The
/// line shape mirrors a change (`path (kind)`), but dimmed so it reads as an
/// access, not a commit; the op (and ask decision) ride in the parenthetical.
fn print_notes(notes: &[Note], root: &Path) {
    println!("\n{}", "not staged:".dimmed());
    for note in notes {
        let (path, kind) = match note {
            Note::Block { path, op } => (path, format!("blocked {}", op.label())),
            Note::Ask { path, op, decision } => {
                (path, format!("asked {} → {decision}", op.label()))
            }
        };
        println!(
            "{} {}",
            rel(path, root).dimmed(),
            format!("({kind})").yellow()
        );
    }
}

/// In the default view (latest snapshot, something staged), point at the
/// vs-base range. Shares its shape with `yolo journal`'s footer.
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

/// Human-readable suffix for "No changes…" — names the queried range.
fn range_label(spec: Option<&str>) -> String {
    match spec {
        None => " staged".into(),
        Some(s) => format!(" in `{s}`"),
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

    // ── id / range grammar ───────────────────────────────────────────

    #[test]
    fn range_label_names_the_spec() {
        assert_eq!(range_label(None), " staged");
        assert_eq!(range_label(Some("3..5")), " in `3..5`");
    }
}

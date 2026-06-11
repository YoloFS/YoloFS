// yolo CLI — changeset.rs
//
// The model behind `yolo review` and the post-`yolo run -- <cmd>` review: what
// changed across a span of the journal. Rendering lives in cmd/review.rs; this
// file only resolves *what* happened, not *how* to show it.

use crate::journal::{DirTree, Journal, Note, Record, Target};
use std::collections::HashSet;

/// One net change at a path. `start` is the range-start old side (what `--diff`
/// reads for the old content); `end` is the net overlay state. Their pairing
/// classifies added/modified/deleted/renamed — no rebuilt previous tree, no base
/// stat. Both come straight off the folded tree node.
pub struct Change {
    pub path: String,
    pub start: Option<Target>,
    pub end: Option<Target>,
}

/// What changed across a span of snapshots: the net per-path effect (what
/// `commit` would apply) plus the observational notes seen along the way.
///
/// The *same* structure describes the changes between two adjacent snapshots or
/// across many: intermediate snapshots collapse into the net result.
pub struct Changeset {
    /// Net per-path changes (vacated rename sources already dropped).
    pub changes: Vec<Change>,
    /// Observational A/B notes (deduped) — an audit overlay, not staged changes.
    pub notes: Vec<Note>,
}

impl Changeset {
    /// Resolve the net changes in segments `[start, end)`. Borrows the journal so
    /// `--each` can call it once per segment.
    pub fn collect(journal: &Journal, start: usize, end: usize) -> Self {
        // One O(segment) pass over the live records collects the observational
        // A/B accesses, deduped (a summary shouldn't repeat what `yolo journal`
        // lists in full). The start/end old-side + net state come from the folded
        // tree below — no separate pre-image side map.
        let mut seen = HashSet::new();
        let mut notes = Vec::new();
        for i in start..end {
            if !journal.is_alive(i) {
                continue;
            }
            for record in &journal.segments[i].records {
                // Notes only; the net state comes from the folded tree below.
                let Record::Note(n) = record else { continue };
                if seen.insert(note_key(n)) {
                    notes.push(n.clone());
                }
            }
        }

        // Fold the range into one tree → start/end per path. Borrowed, so the
        // journal isn't consumed — `--each` calls `collect` once per segment.
        let tree = DirTree::build(journal.live_segments_range(start, end));
        let mut all = Vec::new();
        tree.for_each_change(|p, s, e| {
            all.push((p.to_string(), s.cloned(), e.cloned()));
        });

        // A plain rename renders as a single "(renamed)" line, so drop the
        // vacated source it leaves behind. Key on a *surviving* base-path
        // destination: in `mv a b; rm b` the destination is deleted, so no
        // `end = BasePath` survives and `/a`'s delete is *not* suppressed.
        let moved_to: HashSet<&str> = all
            .iter()
            .filter_map(|(_, _, e)| match e {
                Some(Target::BasePath(l)) => Some(l.as_str()),
                _ => None,
            })
            .collect();

        let mut changes = Vec::new();
        for (p, start, end) in &all {
            let vacated_source = matches!(end, Some(Target::Absence))
                && matches!(start, Some(Target::BasePath(l)) if moved_to.contains(l.as_str()));
            if vacated_source {
                continue; // already shown by the "(renamed)" line
            }
            changes.push(Change {
                path: p.clone(),
                start: start.clone(),
                end: end.clone(),
            });
        }

        Changeset { changes, notes }
    }
}

/// A dedup key for a note: its kind, path, op, and (for asks) decision.
fn note_key(note: &Note) -> String {
    match note {
        Note::Block { path, op } => format!("B\0{path}\0{}", op.label()),
        Note::Ask { path, op, decision } => format!("A\0{path}\0{}\0{decision}", op.label()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::journal::{Action, Journal, Target};

    fn collect(records: Vec<Record>) -> Changeset {
        let journal = Journal::new(records);
        let end = journal.segments.len();
        Changeset::collect(&journal, 0, end)
    }

    fn base(p: &str) -> Target {
        Target::BasePath(p.into())
    }

    fn stage(path: &str, ino: u32, pre: Target) -> Record {
        Record::Action(Action::Stage {
            path: path.into(),
            ino,
            pre,
        })
    }

    fn delete(path: &str, pre: Target) -> Record {
        Record::Action(Action::Delete {
            path: path.into(),
            pre,
        })
    }

    fn rename(dst: &str, src: &str, src_pre: Target, dst_pre: Target) -> Record {
        Record::Action(Action::Rename {
            src: src.into(),
            dst: dst.into(),
            src_pre,
            dst_pre,
        })
    }

    fn find<'a>(cs: &'a Changeset, path: &str) -> Option<&'a Change> {
        cs.changes.iter().find(|c| c.path == path)
    }

    #[test]
    fn create_has_absent_start() {
        // Fresh create: start = Absence (no old side) ⇒ classifies as added.
        let cs = collect(vec![stage("/a", 1, Target::Absence)]);
        let c = find(&cs, "/a").unwrap();
        assert!(matches!(c.start, Some(Target::Absence)));
        assert!(matches!(c.end, Some(Target::StagedFile(1))));
    }

    #[test]
    fn modify_carries_base_start() {
        let cs = collect(vec![stage("/a", 1, base("/a"))]);
        assert!(
            matches!(find(&cs, "/a").unwrap().start, Some(Target::BasePath(ref p)) if p == "/a")
        );
    }

    #[test]
    fn delete_carries_start() {
        let cs = collect(vec![delete("/a", base("/a"))]);
        let c = find(&cs, "/a").unwrap();
        assert!(matches!(c.start, Some(Target::BasePath(ref p)) if p == "/a"));
        assert!(matches!(c.end, Some(Target::Absence)));
    }

    #[test]
    fn first_touch_wins_create_delete_recreate() {
        // Create (Absence), delete, recreate: the first touch decides, so start =
        // Absence ⇒ "added", not "modified".
        let cs = collect(vec![
            stage("/a", 1, Target::Absence),
            delete("/a", base("/a")),
            stage("/a", 2, Target::Absence),
        ]);
        let c = find(&cs, "/a").unwrap();
        assert!(matches!(c.start, Some(Target::Absence)));
        assert!(matches!(c.end, Some(Target::StagedFile(2))));
    }

    #[test]
    fn create_then_delete_nets_to_absent() {
        // Net tombstone, but first touch (the create) had no old side — review.rs
        // classifies start=Absence, end=Absence as a no-op and skips it.
        let cs = collect(vec![
            stage("/a", 1, Target::Absence),
            delete("/a", base("/a")),
        ]);
        let c = find(&cs, "/a").unwrap();
        assert!(matches!(c.start, Some(Target::Absence)));
        assert!(matches!(c.end, Some(Target::Absence)));
    }

    #[test]
    fn rename_shows_single_entry() {
        // mv /a /b (base file): only /b (renamed) appears; the vacated /a is
        // suppressed because /b's surviving end = BasePath("/a").
        let cs = collect(vec![rename("/b", "/a", base("/a"), Target::Absence)]);
        assert!(
            find(&cs, "/a").is_none(),
            "vacated source should be suppressed"
        );
        assert!(matches!(
            find(&cs, "/b").unwrap().end,
            Some(Target::BasePath(_))
        ));
    }

    #[test]
    fn rename_then_delete_keeps_source_delete() {
        // mv /a /b; rm /b: no surviving end = BasePath, so /a's delete is NOT
        // suppressed — review still shows /a deleted.
        let cs = collect(vec![
            rename("/b", "/a", base("/a"), Target::Absence),
            delete("/b", base("/a")),
        ]);
        let a = find(&cs, "/a").unwrap();
        assert!(matches!(a.start, Some(Target::BasePath(ref p)) if p == "/a"));
        assert!(matches!(a.end, Some(Target::Absence)));
    }
}

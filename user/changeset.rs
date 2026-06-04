// yolo CLI — changeset.rs
//
// The model behind `status` / `diff` / the `yolo -- <cmd>` review: what changed
// across a span of the journal. Rendering lives in diff.rs; this file only
// resolves *what* happened, not *how* to show it.

use crate::journal::{Action, DirTree, Journal, Note, Record, Target};
use std::collections::{HashMap, HashSet};

/// One net change. `preimage` is the absolute path of the content that was at
/// this path at the start of the range (the previous snapshot), or `None` if
/// nothing was. Its presence classifies added/modified/deleted; `diff` reads it
/// for the old content. Both status and diff work from this alone — no rebuilt
/// previous tree.
pub struct Change {
    pub path: String,
    pub target: Target,
    pub preimage: Option<String>,
}

/// What changed across a span of snapshots: the net per-path effect (what
/// `commit` would apply) plus the observational notes seen along the way.
///
/// The *same* structure describes the changes between two adjacent snapshots or
/// across many: intermediate snapshots collapse into the net result.
pub struct Changeset {
    /// Net per-path changes (rename vacates already dropped), each carrying its
    /// first-touch pre-image.
    pub changes: Vec<Change>,
    /// Observational A/B notes (deduped) — an audit overlay, not staged changes.
    pub notes: Vec<Note>,
}

impl Changeset {
    /// Resolve the net changes in segments `[start, end)`, keeping only `path`
    /// when given. Borrows the journal so `--each` can call it once per segment.
    pub fn collect(journal: &Journal, start: usize, end: usize, path: Option<&str>) -> Self {
        // One O(segment) pass over the live records collects:
        //   * `notes` — observational A/B accesses, deduped (a summary shouldn't
        //     repeat what `yolo audit` lists in full).
        //   * `preimage` — per path, the pre-image from its *first* touch in the
        //     range. First-touch (not the net action) is what matters: a
        //     create+delete must let the create's "no pre-image" win, and for a
        //     multi-segment range the old content is the range-start version. A
        //     stage carries the kernel's pre-image; a delete carries the removed
        //     content's path; a rename-dest is created by the move (no pre-image).
        let mut seen = HashSet::new();
        let mut notes = Vec::new();
        let mut preimage: HashMap<String, Option<String>> = HashMap::new();
        for i in start..end {
            if !journal.is_alive(i) {
                continue;
            }
            for record in &journal.segments[i].records {
                match record {
                    Record::Note(n) => {
                        if seen.insert(note_key(n)) {
                            notes.push(n.clone());
                        }
                    }
                    Record::Action(Action::Stage {
                        path, preimage: p, ..
                    }) => {
                        preimage.entry(path.clone()).or_insert_with(|| p.clone());
                    }
                    Record::Action(Action::Delete { path, preimage: p }) => {
                        preimage.entry(path.clone()).or_insert_with(|| p.clone());
                    }
                    Record::Action(Action::Rename { dst, .. }) => {
                        preimage.entry(dst.clone()).or_insert(None);
                    }
                    // Markers split segments and never appear inside one.
                    Record::Marker(_) => {}
                }
            }
        }

        // Replay the range into one tree → the net change per path. Borrowed,
        // so the journal isn't consumed — `--each` calls `collect` once per
        // segment on the same journal.
        let tree = DirTree::build(journal.live_segments_range(start, end));
        let mut all = Vec::new();
        tree.for_each(|p, target| all.push((p.to_string(), target.clone())));

        // A rename renders as a single "(renamed)" entry, so drop the tombstone
        // the net tree leaves at the vacated source — its path is the source of
        // some redirect (BasePath) target.
        let rename_sources: HashSet<&str> = all
            .iter()
            .filter_map(|(_, t)| match t {
                Target::BasePath(src) => Some(src.as_str()),
                _ => None,
            })
            .collect();

        let mut changes = Vec::new();
        for (p, target) in &all {
            if matches!(target, Target::Tombstone) && rename_sources.contains(p.as_str()) {
                continue; // rename vacate — already shown by the "(renamed)" line
            }
            if path.is_some_and(|q| !target.matches_path(p, q)) {
                continue;
            }
            changes.push(Change {
                path: p.clone(),
                target: target.clone(),
                preimage: preimage.get(p).cloned().flatten(),
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
    use crate::journal::Journal;

    fn collect(records: Vec<Record>) -> Changeset {
        let journal = Journal::new(records);
        let end = journal.segments.len();
        Changeset::collect(&journal, 0, end, None)
    }

    fn stage(path: &str, ino: u32, preimage: Option<&str>) -> Record {
        Record::Action(Action::Stage {
            path: path.into(),
            ino,
            preimage: preimage.map(Into::into),
        })
    }

    fn delete(path: &str, preimage: Option<&str>) -> Record {
        Record::Action(Action::Delete {
            path: path.into(),
            preimage: preimage.map(Into::into),
        })
    }

    fn rename(dst: &str, src: &str) -> Record {
        Record::Action(Action::Rename {
            src: src.into(),
            dst: dst.into(),
        })
    }

    fn find<'a>(cs: &'a Changeset, path: &str) -> Option<&'a Change> {
        cs.changes.iter().find(|c| c.path == path)
    }

    #[test]
    fn create_has_no_preimage() {
        let cs = collect(vec![stage("/a", 1, None)]);
        assert!(find(&cs, "/a").unwrap().preimage.is_none());
    }

    #[test]
    fn modify_carries_preimage() {
        let cs = collect(vec![stage("/a", 1, Some("/a"))]);
        assert_eq!(find(&cs, "/a").unwrap().preimage.as_deref(), Some("/a"));
    }

    #[test]
    fn delete_carries_preimage() {
        let cs = collect(vec![delete("/a", Some("/a"))]);
        assert_eq!(find(&cs, "/a").unwrap().preimage.as_deref(), Some("/a"));
    }

    #[test]
    fn first_touch_wins_create_delete_recreate() {
        // Create (no pre-image), delete, recreate: the first touch decides, so no
        // pre-image ⇒ "added", not "modified".
        let cs = collect(vec![
            stage("/a", 1, None),
            delete("/a", Some("/a")),
            stage("/a", 2, None),
        ]);
        assert!(find(&cs, "/a").unwrap().preimage.is_none());
    }

    #[test]
    fn create_then_delete_keeps_no_preimage() {
        // Net tombstone, but first touch (the create) has no pre-image — diff.rs
        // classifies that as a no-op and skips it.
        let cs = collect(vec![stage("/a", 1, None), delete("/a", Some("/a"))]);
        let c = find(&cs, "/a").unwrap();
        assert!(matches!(c.target, Target::Tombstone));
        assert!(c.preimage.is_none());
    }

    #[test]
    fn rename_shows_single_entry() {
        // mv /a /b: only /b (renamed) appears; the vacated /a tombstone is dropped.
        let cs = collect(vec![rename("/b", "/a")]);
        assert!(
            find(&cs, "/a").is_none(),
            "vacated source should be dropped"
        );
        assert!(matches!(
            find(&cs, "/b").unwrap().target,
            Target::BasePath(_)
        ));
    }
}

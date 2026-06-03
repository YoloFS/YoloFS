// yolo CLI — changeset.rs
//
// The model behind `status` / `diff` / the `yolo -- <cmd>` review: what changed
// across a span of the journal. Rendering lives in diff.rs; this file only
// resolves *what* happened, not *how* to show it.

use crate::journal::{Action, DirTree, Journal, Note, Record, Target};
use std::collections::{HashMap, HashSet};

/// What changed across a span of snapshots: the net effect on each path versus
/// the base filesystem — exactly what `commit` would apply — plus the
/// observational notes seen along the way and a per-path "existed at the start
/// of the range" map for vs-previous-snapshot classification.
///
/// The *same* structure describes the changes between two adjacent snapshots or
/// across many: intermediate snapshots collapse into the net result, so the
/// range is the only thing that varies.
pub struct Changeset {
    /// Each net-changed path and its resolved target (vs base).
    pub changes: Vec<(String, Target)>,
    /// Observational A/B notes (deduped) — an audit overlay, not staged changes.
    pub notes: Vec<Note>,
    /// For each path touched in the range, whether it existed at the range's
    /// start (the previous snapshot). Lets `status` classify added/modified/
    /// deleted without rebuilding the previous tree — see [`collect`].
    pub prev_present: HashMap<String, bool>,
}

impl Changeset {
    /// Resolve the net changes in segments `[start, end)` (consuming the
    /// journal), keeping only `path` when given.
    pub fn collect(journal: Journal, start: usize, end: usize, path: Option<&str>) -> Self {
        // One O(segment) pass over the live range collects, from the raw records
        // (before the journal is consumed for the tree build below):
        //
        //   * `notes` — observational A/B accesses, deduped (a summary shouldn't
        //     repeat what `yolo audit` lists in full).
        //   * `prev_present` — for each touched path, whether it existed at the
        //     start of the range (i.e. in the previous snapshot). Taken from the
        //     *first* touch of each path: a stage carries the kernel's
        //     redirect-resolved `existed` bit; a delete or rename-source can only
        //     act on something that already existed; a rename-dest is created by
        //     the rename. This is what lets the default vs-previous-snapshot
        //     status classify the latest segment alone — O(segment), not the
        //     O(journal) cost of rebuilding the previous tree.
        let mut seen = HashSet::new();
        let mut notes = Vec::new();
        let mut prev_present: HashMap<String, bool> = HashMap::new();
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
                    Record::Action(Action::Stage { path, existed, .. }) => {
                        prev_present.entry(path.clone()).or_insert(*existed);
                    }
                    Record::Action(Action::Delete { path }) => {
                        prev_present.entry(path.clone()).or_insert(true);
                    }
                    Record::Action(Action::Rename { src, dst }) => {
                        prev_present.entry(src.clone()).or_insert(true);
                        prev_present.entry(dst.clone()).or_insert(false);
                    }
                    // Markers split segments and never appear inside one.
                    Record::Marker(_) => {}
                }
            }
        }

        // Replay the whole range into one tree → the net change per path.
        let tree = DirTree::build(journal.into_live_segments_range(start, end));
        let mut changes = Vec::new();
        tree.for_each(|p, target| {
            if path.is_none() || target.matches_path(p, path.unwrap()) {
                changes.push((p.to_string(), target.clone()));
            }
        });

        Changeset {
            changes,
            notes,
            prev_present,
        }
    }

    /// Whether `path` existed at the start of the range (the previous snapshot).
    /// Paths with no recorded touch — e.g. a staged child swept along by an
    /// in-range directory rename — default to absent, matching how the
    /// previous-tree baseline resolved them (no redirect yet → base miss).
    pub fn present_before(&self, path: &str) -> bool {
        self.prev_present.get(path).copied().unwrap_or(false)
    }
}

/// A dedup key for a note: its kind, path, op, and (for asks) decision.
fn note_key(note: &Note) -> String {
    match note {
        Note::Block { path, op } => format!("B\0{path}\0{}", op.label()),
        Note::Ask {
            path,
            op,
            decision,
        } => format!("A\0{path}\0{}\0{decision}", op.label()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::journal::Journal;

    fn collect(records: Vec<Record>) -> Changeset {
        let journal = Journal::new(records);
        let end = journal.segments.len();
        Changeset::collect(journal, 0, end, None)
    }

    fn stage(path: &str, ino: u32, existed: bool) -> Record {
        Record::Action(Action::Stage {
            path: path.into(),
            ino,
            existed,
        })
    }

    fn delete(path: &str) -> Record {
        Record::Action(Action::Delete { path: path.into() })
    }

    fn rename(dst: &str, src: &str) -> Record {
        Record::Action(Action::Rename {
            src: src.into(),
            dst: dst.into(),
        })
    }

    #[test]
    fn create_is_absent_before() {
        // Fresh create → existed=false → not present in the previous snapshot.
        assert!(!collect(vec![stage("/a", 1, false)]).present_before("/a"));
    }

    #[test]
    fn modify_existing_is_present_before() {
        // Copy-up of a pre-existing/base file → existed=true.
        assert!(collect(vec![stage("/a", 1, true)]).present_before("/a"));
    }

    #[test]
    fn delete_implies_present_before() {
        assert!(collect(vec![delete("/a")]).present_before("/a"));
    }

    #[test]
    fn rename_source_present_dest_absent() {
        let cs = collect(vec![rename("/b", "/a")]);
        assert!(cs.present_before("/a"), "rename source existed");
        assert!(!cs.present_before("/b"), "rename dest is created by the move");
    }

    #[test]
    fn first_touch_wins_create_delete_recreate() {
        // Create (existed=false), delete, recreate: the FIRST touch decides
        // presence, so the path is absent-before — a create+delete nets to a
        // no-op delete, not a real "deleted".
        let cs = collect(vec![stage("/a", 1, false), delete("/a"), stage("/a", 2, false)]);
        assert!(!cs.present_before("/a"));
    }

    #[test]
    fn untouched_path_defaults_absent() {
        assert!(!collect(vec![stage("/a", 1, true)]).present_before("/zzz"));
    }
}

// yolo CLI — changeset.rs
//
// The model behind `status` / `diff` / the `yolo -- <cmd>` review: what changed
// across a span of the journal. Rendering lives in diff.rs; this file only
// resolves *what* happened, not *how* to show it.

use crate::journal::{DirTree, Journal, Note, Record, Target};

/// What changed across a span of snapshots: the net effect on each path versus
/// the base filesystem — exactly what `commit` would apply — plus the
/// observational notes seen along the way.
///
/// The *same* structure describes the changes between two adjacent snapshots or
/// across many: intermediate snapshots collapse into the net result, so the
/// range is the only thing that varies.
pub struct Changeset {
    /// Each net-changed path and its resolved target (vs base).
    pub changes: Vec<(String, Target)>,
    /// Observational A/B notes (deduped) — an audit overlay, not staged changes.
    pub notes: Vec<Note>,
}

impl Changeset {
    /// Resolve the net changes in segments `[start, end)` (consuming the
    /// journal), keeping only `path` when given.
    pub fn collect(journal: Journal, start: usize, end: usize, path: Option<&str>) -> Self {
        // Observational notes (A/B), deduped across the range — a summary
        // shouldn't repeat what `yolo audit` lists in full. Read from the raw
        // segments before the journal is consumed below.
        let mut seen = std::collections::HashSet::new();
        let notes: Vec<Note> = (start..end)
            .filter(|i| journal.is_alive(*i))
            .flat_map(|i| journal.segments[i].records.iter())
            .filter_map(|r| match r {
                Record::Note(n) => Some(n.clone()),
                _ => None,
            })
            .filter(|n| seen.insert(note_key(n)))
            .collect();

        // Replay the whole range into one tree → the net change per path.
        let tree = DirTree::build(journal.into_live_segments_range(start, end));
        let mut changes = Vec::new();
        tree.for_each(|p, target| {
            if path.is_none() || target.matches_path(p, path.unwrap()) {
                changes.push((p.to_string(), target.clone()));
            }
        });

        Changeset { changes, notes }
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

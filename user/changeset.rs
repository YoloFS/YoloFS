// yolo CLI — changeset.rs
//
// The model behind `yolo review` and the post-`yolo run -- <cmd>` review: what
// changed across a span of the journal. Rendering lives in cmd/review.rs; this
// file only resolves *what* happened, not *how* to show it.

use crate::journal::{DirTree, Journal, Note, Record, Target};
use std::collections::HashSet;

/// One net change at a path. `old` is the range-start `old` side (what `--diff`
/// reads for the old content); `new` is the net overlay state. Their pairing
/// classifies added/modified/deleted/renamed — no rebuilt previous tree, no base
/// stat. Both come straight off the folded tree node.
pub struct Change {
    pub path: String,
    pub old: Option<Target>,
    pub new: Option<Target>,
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
        // lists in full). The `old`/`new` per-path state comes from the folded
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

        // Fold the range into one tree → `old`/`new` per path, flattened into
        // review changes. Borrowed, so the journal isn't consumed — `--each`
        // calls `collect` once per segment.
        let tree = DirTree::build(journal.live_segments_range(start, end));
        let mut changes = Vec::new();
        tree.for_each_change(|p, old, new| {
            changes.push(Change {
                path: p.to_string(),
                old: old.cloned(),
                new: new.cloned(),
            });
        });

        // A base path is a unique content origin, so a vacated source `/a`
        // (`new = Absence`, `old = BasePath(L)`) is a *moved* file, not a delete,
        // exactly when some surviving node still redirects to `L` — then the
        // rename renders as one "(renamed)" line instead of delete + rename.
        // Keying on a *surviving* redirect is what keeps `mv a b; rm b` showing
        // `/a` deleted: `rm b` clears the only `new = BasePath(/a)`, so `/a` stays.
        // Owned (not `&str` into `changes`) so the `retain` below can mutate it.
        let live_redirect_targets: HashSet<String> = changes
            .iter()
            .filter_map(|c| match &c.new {
                Some(Target::BasePath(l)) => Some(l.clone()),
                _ => None,
            })
            .collect();
        changes.retain(|c| {
            let moved_source = matches!(c.new, Some(Target::Absence))
                && matches!(&c.old, Some(Target::BasePath(l)) if live_redirect_targets.contains(l));
            !moved_source
        });

        Changeset { changes, notes }
    }
}

/// A dedup key for a note: its kind, path, op, (for asks) decision, and (for
/// blocks) the rule path — so blocks differing only by which rule fired are
/// kept distinct.
fn note_key(note: &Note) -> String {
    match note {
        Note::Block {
            path,
            op,
            rule_path,
        } => {
            format!("B\0{path}\0{}\0{rule_path}", op.label())
        }
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
        assert!(matches!(c.old, Some(Target::Absence)));
        assert!(matches!(c.new, Some(Target::StagedFile(1))));
    }

    #[test]
    fn modify_carries_base_start() {
        let cs = collect(vec![stage("/a", 1, base("/a"))]);
        assert!(matches!(find(&cs, "/a").unwrap().old, Some(Target::BasePath(ref p)) if p == "/a"));
    }

    #[test]
    fn delete_carries_start() {
        let cs = collect(vec![delete("/a", base("/a"))]);
        let c = find(&cs, "/a").unwrap();
        assert!(matches!(c.old, Some(Target::BasePath(ref p)) if p == "/a"));
        assert!(matches!(c.new, Some(Target::Absence)));
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
        assert!(matches!(c.old, Some(Target::Absence)));
        assert!(matches!(c.new, Some(Target::StagedFile(2))));
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
        assert!(matches!(c.old, Some(Target::Absence)));
        assert!(matches!(c.new, Some(Target::Absence)));
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
            find(&cs, "/b").unwrap().new,
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
        assert!(matches!(a.old, Some(Target::BasePath(ref p)) if p == "/a"));
        assert!(matches!(a.new, Some(Target::Absence)));
    }

    #[test]
    fn blocks_differing_by_rule_path_are_not_deduped() {
        use crate::journal::{Note, Op};
        let block = |rule: &str| {
            Record::Note(Note::Block {
                path: "/a".into(),
                op: Op::Write,
                rule_path: rule.into(),
            })
        };
        // Same path/op but different blocking rules stay distinct; an exact
        // duplicate folds. rule_path is part of the dedup key.
        let cs = collect(vec![block("/x"), block("/y"), block("/x")]);
        assert_eq!(
            cs.notes.len(),
            2,
            "distinct rule_paths kept, dup folded: {:?}",
            cs.notes
        );
    }
}

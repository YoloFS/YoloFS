// yolo CLI — changeset.rs
//
// The model behind `yolo review` and the post-`yolo run -- <cmd>` review: what
// changed across a span of the journal. Rendering lives in cmd/review.rs; this
// file only resolves *what* happened, not *how* to show it.

use crate::journal::{DirTree, Journal, Note, Record, Backing};
use std::collections::HashSet;

/// One net change at a path. `old` is the range-start `old` side (what `--diff`
/// reads for the old content); `new` is the net overlay state. Their pairing
/// classifies added/modified/deleted/renamed — no rebuilt previous tree, no base
/// stat. Both come straight off the folded tree node.
pub struct Change {
    pub path: String,
    pub old: Option<Backing>,
    pub new: Option<Backing>,
}

/// What changed across a span of snapshots: the net per-path effect (what
/// `commit` would apply) plus the observational notes seen along the way.
///
/// The *same* structure describes the changes between two adjacent snapshots or
/// across many: intermediate snapshots collapse into the net result.
pub struct Changeset {
    /// Net per-path changes (vacated rename sources already dropped).
    pub changes: Vec<Change>,
    /// Observational G/C notes — an audit overlay, not staged changes.
    pub notes: Vec<Note>,
}

impl Changeset {
    /// Resolve the net changes in segments `[start, end)`. Borrows the journal so
    /// `--each` can call it once per segment.
    pub fn collect(journal: &Journal, start: usize, end: usize) -> Self {
        // One O(segment) pass collects every selected observational note in
        // journal order. C assignments are never removed by filesystem branch
        // reachability; G accesses follow their segment's liveness.
        let mut notes = Vec::new();
        for i in start..end {
            for record in &journal.segments[i].records {
                // Notes only; the net state comes from the folded tree below.
                let Record::Note(n) = record else { continue };
                if journal.is_record_alive(i, record) {
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
        // (`new = None`, `old = BasePath(L)`) is a *moved* file, not a delete,
        // exactly when some surviving node still redirects to `L` — then the
        // rename renders as one "(renamed)" line instead of delete + rename.
        // Keying on a *surviving* redirect is what keeps `mv a b; rm b` showing
        // `/a` deleted: `rm b` clears the only `new = BasePath(/a)`, so `/a` stays.
        // Owned (not `&str` into `changes`) so the `retain` below can mutate it.
        let live_redirect_targets: HashSet<String> = changes
            .iter()
            .filter_map(|c| match &c.new {
                Some(Backing::BasePath(l)) => Some(l.clone()),
                _ => None,
            })
            .collect();
        changes.retain(|c| {
            let moved_source = matches!(c.new, Some(Backing::None))
                && matches!(&c.old, Some(Backing::BasePath(l)) if live_redirect_targets.contains(l));
            !moved_source
        });

        Changeset { changes, notes }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::journal::{Action, Journal, Backing};

    fn collect(records: Vec<Record>) -> Changeset {
        let journal = Journal::new(records);
        let end = journal.segments.len();
        Changeset::collect(&journal, 0, end)
    }

    fn base(p: &str) -> Backing {
        Backing::BasePath(p.into())
    }

    fn stage(path: &str, ino: u32, pre: Backing) -> Record {
        Record::Action(Action::Stage {
            path: path.into(),
            ino,
            pre,
        })
    }

    fn delete(path: &str, pre: Backing) -> Record {
        Record::Action(Action::Delete {
            path: path.into(),
            pre,
        })
    }

    fn rename(dst: &str, src: &str, src_pre: Backing, dst_pre: Backing) -> Record {
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
        // Fresh create: start = None (no old side) ⇒ classifies as added.
        let cs = collect(vec![stage("/a", 1, Backing::None)]);
        let c = find(&cs, "/a").unwrap();
        assert!(matches!(c.old, Some(Backing::None)));
        assert!(matches!(c.new, Some(Backing::StagedFile(1))));
    }

    #[test]
    fn modify_carries_base_start() {
        let cs = collect(vec![stage("/a", 1, base("/a"))]);
        assert!(matches!(find(&cs, "/a").unwrap().old, Some(Backing::BasePath(ref p)) if p == "/a"));
    }

    #[test]
    fn delete_carries_start() {
        let cs = collect(vec![delete("/a", base("/a"))]);
        let c = find(&cs, "/a").unwrap();
        assert!(matches!(c.old, Some(Backing::BasePath(ref p)) if p == "/a"));
        assert!(matches!(c.new, Some(Backing::None)));
    }

    #[test]
    fn first_touch_wins_create_delete_recreate() {
        // Create (None), delete, recreate: the first touch decides, so start =
        // None ⇒ "added", not "modified".
        let cs = collect(vec![
            stage("/a", 1, Backing::None),
            delete("/a", base("/a")),
            stage("/a", 2, Backing::None),
        ]);
        let c = find(&cs, "/a").unwrap();
        assert!(matches!(c.old, Some(Backing::None)));
        assert!(matches!(c.new, Some(Backing::StagedFile(2))));
    }

    #[test]
    fn create_then_delete_nets_to_absent() {
        // Net tombstone, but first touch (the create) had no old side — review.rs
        // classifies start=None, end=None as a no-op and skips it.
        let cs = collect(vec![
            stage("/a", 1, Backing::None),
            delete("/a", base("/a")),
        ]);
        let c = find(&cs, "/a").unwrap();
        assert!(matches!(c.old, Some(Backing::None)));
        assert!(matches!(c.new, Some(Backing::None)));
    }

    #[test]
    fn rename_shows_single_entry() {
        // mv /a /b (base file): only /b (renamed) appears; the vacated /a is
        // suppressed because /b's surviving end = BasePath("/a").
        let cs = collect(vec![rename("/b", "/a", base("/a"), Backing::None)]);
        assert!(
            find(&cs, "/a").is_none(),
            "vacated source should be suppressed"
        );
        assert!(matches!(
            find(&cs, "/b").unwrap().new,
            Some(Backing::BasePath(_))
        ));
    }

    #[test]
    fn rename_then_delete_keeps_source_delete() {
        // mv /a /b; rm /b: no surviving end = BasePath, so /a's delete is NOT
        // suppressed — review still shows /a deleted.
        let cs = collect(vec![
            rename("/b", "/a", base("/a"), Backing::None),
            delete("/b", base("/a")),
        ]);
        let a = find(&cs, "/a").unwrap();
        assert!(matches!(a.old, Some(Backing::BasePath(ref p)) if p == "/a"));
        assert!(matches!(a.new, Some(Backing::None)));
    }

    #[test]
    fn repeated_gates_and_configurations_stay_ordered() {
        use crate::journal::{GateResult, Note, Op, Policy};
        let gate = || {
            Record::Note(Note::Gate {
                path: "/a".into(),
                op: Op::Write,
                result: GateResult::DirectDeny,
            })
        };
        let configure = |policy| {
            Record::Note(Note::Configure {
                path: "/a".into(),
                policy,
            })
        };
        let cs = collect(vec![
            gate(),
            gate(),
            configure(Policy::Allow),
            configure(Policy::Allow),
            configure(Policy::Deny),
        ]);
        assert_eq!(cs.notes.len(), 5, "all notes must be kept: {:?}", cs.notes);
        assert!(matches!(
            &cs.notes[..],
            [
                Note::Gate {
                    result: GateResult::DirectDeny,
                    ..
                },
                Note::Gate {
                    result: GateResult::DirectDeny,
                    ..
                },
                Note::Configure {
                    policy: Policy::Allow,
                    ..
                },
                Note::Configure {
                    policy: Policy::Allow,
                    ..
                },
                Note::Configure {
                    policy: Policy::Deny,
                    ..
                },
            ]
        ));
    }

    #[test]
    fn configure_survives_dead_segment_but_gate_does_not() {
        use crate::journal::{GateResult, Marker, Note, Op, Policy};
        let cs = collect(vec![
            Record::Marker(Marker::Snapshot { name: "one".into() }),
            Record::Note(Note::Configure {
                path: "/a".into(),
                policy: Policy::Deny,
            }),
            Record::Note(Note::Gate {
                path: "/a/x".into(),
                op: Op::Read,
                result: GateResult::DirectDeny,
            }),
            Record::Marker(Marker::Snapshot { name: "two".into() }),
            Record::Marker(Marker::Travel { target_gen: 1 }),
        ]);
        assert_eq!(cs.notes.len(), 1);
        assert!(matches!(
            &cs.notes[0],
            Note::Configure {
                path,
                policy: Policy::Deny
            } if path == "/a"
        ));
    }
}

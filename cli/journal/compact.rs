// agfs CLI — journal/compact.rs
//
// Compact a sequence of journal records into an ordered ActionList
// that is directly replayable on the base filesystem.
//
// Rules (applied in order):
//   1. Decompose rename+modify: RDR(a,b) + MOD(b, ino) → DEL(a) + ADD(b, ino);
//      REP(a,b) + MOD(b, ino) → DEL(a) + MOD(b, ino).
//   2. Cancel: ADD(x) + DEL(x) → removed; MOD(x) + DEL(x) → DEL(x).
//   3. Merge modifies: MOD(x, ino=1) + MOD(x, ino=2) → MOD(x, ino=2).

use super::types::{Action, ActionList, DType, Record};
use std::collections::{HashMap, HashSet};

/// Convert journal records into a compacted, ordered ActionList.
pub fn compact(records: Vec<Record>) -> ActionList {
    let mut actions: Vec<Action> = records
        .into_iter()
        .filter_map(|r| match r {
            Record::Added { path, dtype, ino } => Some(Action::Add {
                path,
                ino,
                dtype: dtype.unwrap_or(DType::File),
            }),
            Record::Modified { path, dtype, ino } => Some(Action::Modify {
                path,
                ino,
                dtype: dtype.unwrap_or(DType::File),
            }),
            Record::Deleted { path } => Some(Action::Delete { path }),
            Record::Redirect { old, new, dtype } => Some(Action::Rename {
                old,
                new,
                dtype: dtype.unwrap_or(DType::File),
            }),
            Record::Replace { old, new, dtype } => Some(Action::Replace {
                old,
                new,
                dtype: dtype.unwrap_or(DType::File),
            }),
            Record::Checkpoint { .. } | Record::Restore { .. } => None,
        })
        .collect();

    decompose_rename_modify(&mut actions);
    cancel(&mut actions);
    merge_modifies(&mut actions);

    ActionList(actions)
}

// ── Pass 1: Decompose rename+modify ──────────────────────────────────
//
// When a Rename/Replace is followed by a Modify at the same destination,
// decompose:
//   RDR(a, b) + MOD(b, ino) → DEL(a) + ADD(b, ino)   (b is new to base)
//   REP(a, b) + MOD(b, ino) → DEL(a) + MOD(b, ino)   (b existed in base)

fn decompose_rename_modify(actions: &mut [Action]) {
    let mut rename_dest: HashMap<&str, usize> = HashMap::new();
    let mut pairs: Vec<(usize, usize)> = Vec::new();

    for (i, action) in actions.iter().enumerate() {
        match action {
            Action::Rename { new, .. } | Action::Replace { new, .. } => {
                rename_dest.insert(new, i);
            }
            Action::Modify { path, .. } => {
                if let Some(rename_idx) = rename_dest.remove(path.as_str()) {
                    pairs.push((rename_idx, i));
                }
            }
            Action::Delete { path } | Action::Add { path, .. } => {
                rename_dest.remove(path.as_str());
            }
        }
    }

    for (rename_idx, modify_idx) in pairs {
        let old_action = std::mem::replace(
            &mut actions[rename_idx],
            Action::Delete { path: String::new() },
        );
        let (old, is_replace) = match old_action {
            Action::Replace { old, .. } => (old, true),
            Action::Rename { old, .. } => (old, false),
            _ => unreachable!(),
        };
        actions[rename_idx] = Action::Delete { path: old };
        if !is_replace {
            // RDR(a,b) + MOD(b, ino) → DEL(a) + ADD(b, ino)
            if let Action::Modify { path, ino, dtype } = &actions[modify_idx] {
                actions[modify_idx] = Action::Add {
                    path: path.clone(),
                    ino: *ino,
                    dtype: *dtype,
                };
            }
        }
        // REP(a,b) + MOD(b, ino) → DEL(a) + MOD(b, ino) — Modify stays as-is
    }
}

// ── Pass 2: Cancel ───────────────────────────────────────────────────
//
// ADD(x, ino) + DEL(x) → removed.
// MOD(x, ino) + DEL(x) → DEL(x).

fn cancel(actions: &mut Vec<Action>) {
    let mut add_at: HashMap<&str, usize> = HashMap::new();
    let mut to_remove: HashSet<usize> = HashSet::new();

    for i in 0..actions.len() {
        match &actions[i] {
            Action::Add { path, .. } | Action::Modify { path, .. } => {
                add_at.insert(path, i);
            }
            Action::Delete { path } => {
                if let Some(add_idx) = add_at.remove(path.as_str()) {
                    if matches!(&actions[add_idx], Action::Modify { .. }) {
                        // MOD(x) + DEL(x) → DEL(x) — remove MOD, keep DEL
                        to_remove.insert(add_idx);
                    } else {
                        // ADD(x) + DEL(x) → removed — remove both
                        to_remove.insert(add_idx);
                        to_remove.insert(i);
                    }
                }
            }
            Action::Rename { old, new, .. } | Action::Replace { old, new, .. } => {
                add_at.remove(old.as_str());
                add_at.remove(new.as_str());
            }
        }
    }

    remove_indices(actions, to_remove);
}

// ── Pass 3: Merge modifies ──────────────────────────────────────────
//
// MOD(x, ino=1) + MOD(x, ino=2) → MOD(x, ino=2).

fn merge_modifies(actions: &mut Vec<Action>) {
    let mut modify_at: HashMap<&str, usize> = HashMap::new();
    let mut to_remove: HashSet<usize> = HashSet::new();

    for (i, action) in actions.iter().enumerate() {
        if let Action::Modify { path, .. } = action
            && let Some(prev_idx) = modify_at.insert(path, i)
        {
            to_remove.insert(prev_idx);
        }
    }

    remove_indices(actions, to_remove);
}

// ── Helpers ──────────────────────────────────────────────────────────

fn remove_indices(actions: &mut Vec<Action>, to_remove: HashSet<usize>) {
    if to_remove.is_empty() {
        return;
    }
    let mut sorted: Vec<usize> = to_remove.into_iter().collect();
    sorted.sort_unstable();
    for &idx in sorted.iter().rev() {
        actions.remove(idx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Cancel ───────────────────────────────────────────────────────

    #[test]
    fn add_then_delete_cancels() {
        let records = vec![
            Record::Added {
                path: "/x".into(),
                dtype: Some(DType::File),
                ino: 1,
            },
            Record::Deleted { path: "/x".into() },
        ];
        let al = compact(records);
        assert!(al.0.is_empty(), "A+D should cancel, got: {:?}", al.0);
    }

    #[test]
    fn modify_then_delete_keeps_delete() {
        let records = vec![
            Record::Modified {
                path: "/x".into(),
                dtype: Some(DType::File),
                ino: 1,
            },
            Record::Deleted { path: "/x".into() },
        ];
        let al = compact(records);
        assert_eq!(al.0.len(), 1);
        assert!(matches!(&al.0[0], Action::Delete { path } if path == "/x"));
    }

    // ── Staged rename cancel ─────────────────────────────────────────

    #[test]
    fn staged_rename_cancels_old_add() {
        // touch x (staged), mv x y → kernel emits: ADD(x,1) + DEL(x) + ADD(y,1)
        let records = vec![
            Record::Added {
                path: "/x".into(),
                dtype: Some(DType::File),
                ino: 1,
            },
            Record::Deleted { path: "/x".into() },
            Record::Added {
                path: "/y".into(),
                dtype: Some(DType::File),
                ino: 1,
            },
        ];
        let al = compact(records);
        assert_eq!(al.0.len(), 1, "expected single Add at /y, got: {:?}", al.0);
        assert!(matches!(&al.0[0], Action::Add { path, ino: 1, .. } if path == "/y"));
    }

    // ── Merge modifies ──────────────────────────────────────────────

    #[test]
    fn merge_duplicate_modifies() {
        let records = vec![
            Record::Modified {
                path: "/x".into(),
                dtype: Some(DType::File),
                ino: 1,
            },
            Record::Modified {
                path: "/x".into(),
                dtype: Some(DType::File),
                ino: 2,
            },
        ];
        let al = compact(records);
        assert_eq!(al.0.len(), 1);
        assert!(matches!(&al.0[0], Action::Modify { ino: 2, .. }));
    }

    // ── Decompose rename+modify ─────────────────────────────────────

    #[test]
    fn rename_then_modify_decomposes() {
        let records = vec![
            Record::Redirect {
                old: "/a".into(),
                new: "/b".into(),
                dtype: Some(DType::File),
            },
            Record::Modified {
                path: "/b".into(),
                dtype: Some(DType::File),
                ino: 5,
            },
        ];
        let al = compact(records);
        assert_eq!(al.0.len(), 2, "expected D(a) + A(b,5), got: {:?}", al.0);
        assert!(matches!(&al.0[0], Action::Delete { path } if path == "/a"));
        assert!(
            matches!(&al.0[1], Action::Add { path, ino: 5, .. } if path == "/b"),
            "R+M should decompose to D+A, got: {:?}",
            al.0[1]
        );
    }

    #[test]
    fn replace_then_modify_decomposes() {
        let records = vec![
            Record::Replace {
                old: "/a".into(),
                new: "/b".into(),
                dtype: Some(DType::File),
            },
            Record::Modified {
                path: "/b".into(),
                dtype: Some(DType::File),
                ino: 5,
            },
        ];
        let al = compact(records);
        assert_eq!(al.0.len(), 2, "expected D(a) + M(b,5), got: {:?}", al.0);
        assert!(matches!(&al.0[0], Action::Delete { path } if path == "/a"));
        assert!(
            matches!(&al.0[1], Action::Modify { path, ino: 5, .. } if path == "/b"),
            "P+M should decompose to D+M, got: {:?}",
            al.0[1]
        );
    }

    // ── Independent renames ──────────────────────────────────────────

    #[test]
    fn independent_renames_stay_separate() {
        let records = vec![
            Record::Redirect {
                old: "/a".into(),
                new: "/b".into(),
                dtype: Some(DType::File),
            },
            Record::Redirect {
                old: "/c".into(),
                new: "/d".into(),
                dtype: Some(DType::File),
            },
        ];
        let al = compact(records);
        assert_eq!(al.0.len(), 2);
    }

    // ── Empty input ─────────────────────────────────────────────────

    #[test]
    fn compact_empty() {
        let al = compact(vec![]);
        assert!(al.0.is_empty());
    }

    // ── CKP/RST records filtered ────────────────────────────────────────

    #[test]
    fn checkpoint_and_restore_filtered() {
        let records = vec![
            Record::Added {
                path: "/a".into(),
                dtype: Some(DType::File),
                ino: 1,
            },
            Record::Checkpoint {
                gen_id: 1,
                name: "test".into(),
            },
            Record::Restore {
                gen_id: 2,
                target_gen: 1,
            },
        ];
        let al = compact(records);
        assert_eq!(al.0.len(), 1);
        assert!(matches!(&al.0[0], Action::Add { path, .. } if path == "/a"));
    }
    // ── Decompose cancellation on intervening action ────────────────

    #[test]
    fn intervening_add_prevents_decompose() {
        // RDR(a,b) + ADD(b,5) + MOD(b,7): the Add at /b should cancel
        // the rename-dest tracking so MOD(b) is NOT decomposed with RDR(a,b).
        let records = vec![
            Record::Redirect {
                old: "/a".into(),
                new: "/b".into(),
                dtype: Some(DType::File),
            },
            Record::Added {
                path: "/b".into(),
                dtype: Some(DType::File),
                ino: 5,
            },
            Record::Modified {
                path: "/b".into(),
                dtype: Some(DType::File),
                ino: 7,
            },
        ];
        let al = compact(records);
        assert!(
            al.0.iter().any(|a| matches!(a, Action::Rename { old, new, .. } if old == "/a" && new == "/b")),
            "rename should survive intact, got: {:?}",
            al.0
        );
    }

    #[test]
    fn intervening_delete_prevents_decompose() {
        // RDR(a,b) + DEL(b) + MOD(b,5): the Delete at /b should cancel
        // the rename-dest tracking so MOD(b) is NOT decomposed with RDR(a,b).
        let records = vec![
            Record::Redirect {
                old: "/a".into(),
                new: "/b".into(),
                dtype: Some(DType::File),
            },
            Record::Deleted { path: "/b".into() },
            Record::Modified {
                path: "/b".into(),
                dtype: Some(DType::File),
                ino: 5,
            },
        ];
        let al = compact(records);
        // RDR(a,b) stays as Rename, DEL(b) cancels with MOD(b), leaving just the rename.
        assert!(
            al.0.iter().any(|a| matches!(a, Action::Rename { old, new, .. } if old == "/a" && new == "/b")),
            "rename should survive intact, got: {:?}",
            al.0
        );
    }

    // ── Replace overwrite tracking ──────────────────────────────────

    #[test]
    fn replace_produces_replace_action() {
        let records = vec![Record::Replace {
            old: "/a".into(),
            new: "/b".into(),
            dtype: Some(DType::File),
        }];
        let al = compact(records);
        assert_eq!(al.0.len(), 1);
        assert!(matches!(&al.0[0], Action::Replace { old, new, .. } if old == "/a" && new == "/b"));
    }

    // ── DType::Dir coverage ─────────────────────────────────────────

    #[test]
    fn dir_add_then_delete_cancels() {
        let records = vec![
            Record::Added {
                path: "/d".into(),
                dtype: Some(DType::Dir),
                ino: 1,
            },
            Record::Deleted { path: "/d".into() },
        ];
        let al = compact(records);
        assert!(al.0.is_empty(), "A(dir)+D should cancel, got: {:?}", al.0);
    }

    #[test]
    fn dir_rename_then_modify_decomposes() {
        let records = vec![
            Record::Redirect {
                old: "/a".into(),
                new: "/b".into(),
                dtype: Some(DType::Dir),
            },
            Record::Modified {
                path: "/b".into(),
                dtype: Some(DType::Dir),
                ino: 5,
            },
        ];
        let al = compact(records);
        assert_eq!(al.0.len(), 2, "expected D(a) + A(b,5), got: {:?}", al.0);
        assert!(matches!(&al.0[0], Action::Delete { path } if path == "/a"));
        assert!(
            matches!(&al.0[1], Action::Add { path, ino: 5, dtype: DType::Dir } if path == "/b"),
            "dir RDR+MOD should decompose to DEL+ADD(dir), got: {:?}",
            al.0[1]
        );
    }

    #[test]
    fn dir_merge_modifies() {
        let records = vec![
            Record::Modified {
                path: "/d".into(),
                dtype: Some(DType::Dir),
                ino: 1,
            },
            Record::Modified {
                path: "/d".into(),
                dtype: Some(DType::Dir),
                ino: 2,
            },
        ];
        let al = compact(records);
        assert_eq!(al.0.len(), 1);
        assert!(matches!(&al.0[0], Action::Modify { ino: 2, dtype: DType::Dir, .. }));
    }

    // ── Replace edge cases ───────────────────────────────────────────

    #[test]
    fn replace_then_delete_at_dest() {
        // REP(a,b) + DEL(b): Delete at destination does not cancel Replace;
        // Rename tracking is cleared so both survive.
        let records = vec![
            Record::Replace {
                old: "/a".into(),
                new: "/b".into(),
                dtype: Some(DType::File),
            },
            Record::Deleted { path: "/b".into() },
        ];
        let al = compact(records);
        assert!(
            al.0.iter().any(|a| matches!(a, Action::Replace { old, new, .. } if old == "/a" && new == "/b")),
            "Replace should survive, got: {:?}",
            al.0
        );
        assert!(
            al.0.iter().any(|a| matches!(a, Action::Delete { path } if path == "/b")),
            "Delete should survive, got: {:?}",
            al.0
        );
    }

    #[test]
    fn replace_then_rename_from_dest() {
        // REP(a,b) + RDR(b,c): Replace followed by Rename from destination.
        let records = vec![
            Record::Replace {
                old: "/a".into(),
                new: "/b".into(),
                dtype: Some(DType::File),
            },
            Record::Redirect {
                old: "/b".into(),
                new: "/c".into(),
                dtype: Some(DType::File),
            },
        ];
        let al = compact(records);
        assert_eq!(al.0.len(), 2, "both actions should survive, got: {:?}", al.0);
        assert!(matches!(&al.0[0], Action::Replace { old, new, .. } if old == "/a" && new == "/b"));
        assert!(matches!(&al.0[1], Action::Rename { old, new, .. } if old == "/b" && new == "/c"));
    }

    // ── Multi-segment resolution (via SegmentedJournal pipeline) ─────
    //
    // These verify that compact().collapse() produces correct results
    // when records span checkpoint boundaries and are filtered by liveness.

    use super::super::segment::SegmentedJournal;
    use super::super::types::RawJournal;

    /// Helper: run the full pipeline on raw records including CKP/RST markers.
    fn resolve_all(records: Vec<Record>) -> Vec<(String, super::super::types::Change)> {
        let sj = SegmentedJournal::new(RawJournal(records));
        let live = sj.live().into_records();
        let al = compact(live);
        al.collapse().0
    }

    #[test]
    fn segments_add_delete_readd_across_checkpoints() {
        // Seg0: ADD(x) | CKP1 | Seg1: DEL(x) | CKP2 | Seg2: ADD(x, new ino)
        let records = vec![
            Record::Added {
                path: "/x".into(),
                dtype: Some(DType::File),
                ino: 1,
            },
            Record::Checkpoint {
                gen_id: 1,
                name: "k1".into(),
            },
            Record::Deleted { path: "/x".into() },
            Record::Checkpoint {
                gen_id: 2,
                name: "k2".into(),
            },
            Record::Added {
                path: "/x".into(),
                dtype: Some(DType::File),
                ino: 2,
            },
        ];
        let cs = resolve_all(records);
        assert!(
            cs.iter()
                .any(|(p, c)| p == "/x"
                    && matches!(c, super::super::types::Change::Added { ino: 2, .. })),
            "x should be Added with ino=2, got: {cs:?}",
        );
    }

    #[test]
    fn segments_base_modify_across_checkpoints() {
        // Seg0: MOD(x, ino=1) | CKP1 | Seg1: MOD(x, ino=2)
        let records = vec![
            Record::Modified {
                path: "/x".into(),
                dtype: Some(DType::File),
                ino: 1,
            },
            Record::Checkpoint {
                gen_id: 1,
                name: "k1".into(),
            },
            Record::Modified {
                path: "/x".into(),
                dtype: Some(DType::File),
                ino: 2,
            },
        ];
        let cs = resolve_all(records);
        assert!(
            cs.iter()
                .any(|(p, c)| p == "/x"
                    && matches!(c, super::super::types::Change::Modified { ino: 2, .. })),
            "x should be Modified with latest ino=2, got: {cs:?}",
        );
    }

    #[test]
    fn segments_delete_in_later_segment() {
        // Seg0: MOD(x, ino=1) | CKP1 | Seg1: DEL(x)
        let records = vec![
            Record::Modified {
                path: "/x".into(),
                dtype: Some(DType::File),
                ino: 1,
            },
            Record::Checkpoint {
                gen_id: 1,
                name: "k1".into(),
            },
            Record::Deleted { path: "/x".into() },
        ];
        let cs = resolve_all(records);
        assert!(
            cs.iter()
                .any(|(p, c)| p == "/x" && matches!(c, super::super::types::Change::Deleted)),
            "x should be Deleted, got: {cs:?}",
        );
    }

    #[test]
    fn segments_rename_across_checkpoints() {
        // Seg0: RDR(a, b) | CKP1 | Seg1: MOD(b, ino=5)
        // After decompose: RDR(a,b) + MOD(b,5) → DEL(a) + ADD(b,5)
        let records = vec![
            Record::Redirect {
                old: "/a".into(),
                new: "/b".into(),
                dtype: Some(DType::File),
            },
            Record::Checkpoint {
                gen_id: 1,
                name: "k1".into(),
            },
            Record::Modified {
                path: "/b".into(),
                dtype: Some(DType::File),
                ino: 5,
            },
        ];
        let cs = resolve_all(records);
        let has_del_a = cs
            .iter()
            .any(|(p, c)| p == "/a" && matches!(c, super::super::types::Change::Deleted));
        let has_add_b = cs.iter().any(|(p, c)| {
            p == "/b" && matches!(c, super::super::types::Change::Added { ino: 5, .. })
        });
        assert!(has_del_a, "a should be Deleted, got: {cs:?}");
        assert!(has_add_b, "b should be Added(ino=5), got: {cs:?}");
    }
    #[test]
    fn segments_restore_kills_dead_segment() {
        // Seg0: ADD(x) | CKP1 | Seg1: ADD(y) | CKP2 | RST3(target=CKP1) | Seg3: ADD(z)
        // Restore to CKP1 means Seg1 is dead.
        let records = vec![
            Record::Added {
                path: "/x".into(),
                dtype: Some(DType::File),
                ino: 1,
            },
            Record::Checkpoint {
                gen_id: 1,
                name: "k1".into(),
            },
            Record::Added {
                path: "/y".into(),
                dtype: Some(DType::File),
                ino: 2,
            },
            Record::Checkpoint {
                gen_id: 2,
                name: "k2".into(),
            },
            Record::Restore {
                gen_id: 3,
                target_gen: 1,
            },
            Record::Added {
                path: "/z".into(),
                dtype: Some(DType::File),
                ino: 3,
            },
        ];
        let cs = resolve_all(records);
        let paths: Vec<&str> = cs.iter().map(|(p, _)| p.as_str()).collect();
        assert!(paths.contains(&"/x"), "x should be live: {paths:?}");
        assert!(
            !paths.contains(&"/y"),
            "y should be dead (restored past): {paths:?}"
        );
        assert!(
            paths.contains(&"/z"),
            "z should be live (post-restore): {paths:?}"
        );
    }

    #[test]
    fn segments_delete_recreate_same_path_across_checkpoints() {
        // Seg0: DEL(x) | CKP1 | Seg1: ADD(x, ino=2)
        let records = vec![
            Record::Deleted { path: "/x".into() },
            Record::Checkpoint {
                gen_id: 1,
                name: "k1".into(),
            },
            Record::Added {
                path: "/x".into(),
                dtype: Some(DType::File),
                ino: 2,
            },
        ];
        let cs = resolve_all(records);
        assert!(
            cs.iter()
                .any(|(p, c)| p == "/x"
                    && matches!(c, super::super::types::Change::Added { ino: 2, .. })),
            "x should be Added(ino=2) after delete+recreate, got: {cs:?}",
        );
    }
}

// agfs CLI — journal/simplify.rs
//
// Simplify a sequence of journal records into an ordered ActionList
// that is directly replayable on the base filesystem.
//
// Rules (applied in order):
//   1. Decompose rename+modify: R(a,b) + M(b, ino) → D(a) + A(b, ino);
//      P(a,b) + M(b, ino) → D(a) + M(b, ino).
//   2. Chain collapse (skip cycles): R(a,b) + R(b,c) → R(a,c).
//   3. Cancel: A(x) + D(x) → removed; M(x) + D(x) → D(x).
//   4. Merge modifies: M(x, ino=1) + M(x, ino=2) → M(x, ino=2).

use super::types::{Action, ActionList, DType, Record};
use std::collections::{HashMap, HashSet};

/// Convert journal records into a simplified, ordered ActionList.
pub fn simplify(records: Vec<Record>) -> ActionList {
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
    chain_collapse(&mut actions);
    cancel(&mut actions);
    merge_modifies(&mut actions);

    ActionList(actions)
}

// ── Pass 1: Decompose rename+modify ──────────────────────────────────
//
// When a Rename/Replace is followed by a Modify at the same destination,
// decompose:
//   R(a, b) + M(b, ino) → D(a) + A(b, ino)   (b is new to base)
//   P(a, b) + M(b, ino) → D(a) + M(b, ino)   (b existed in base)

fn decompose_rename_modify(actions: &mut [Action]) {
    let mut rename_dest: HashMap<String, usize> = HashMap::new();
    let mut pairs: Vec<(usize, usize)> = Vec::new();

    for (i, action) in actions.iter().enumerate() {
        match action {
            Action::Rename { new, .. } | Action::Replace { new, .. } => {
                rename_dest.insert(new.clone(), i);
            }
            Action::Modify { path, .. } => {
                if let Some(rename_idx) = rename_dest.remove(path) {
                    pairs.push((rename_idx, i));
                }
            }
            Action::Delete { path } | Action::Add { path, .. } => {
                rename_dest.remove(path);
            }
        }
    }

    for (rename_idx, modify_idx) in pairs {
        let is_replace = matches!(&actions[rename_idx], Action::Replace { .. });
        let old_action = std::mem::replace(
            &mut actions[rename_idx],
            Action::Delete { path: String::new() },
        );
        let old = match old_action {
            Action::Rename { old, .. } | Action::Replace { old, .. } => old,
            _ => unreachable!(),
        };
        actions[rename_idx] = Action::Delete { path: old };
        if !is_replace {
            // R(a,b) + M(b, ino) → D(a) + A(b, ino)
            if let Action::Modify { path, ino, dtype } = &actions[modify_idx] {
                actions[modify_idx] = Action::Add {
                    path: path.clone(),
                    ino: *ino,
                    dtype: *dtype,
                };
            }
        }
        // P(a,b) + M(b, ino) → D(a) + M(b, ino) — Modify stays as-is
    }
}

// ── Pass 2: Chain collapse (skip cycles) ─────────────────────────────
//
// R(a, b) + R(b, c) → R(a, c).  Placed at the position of the first
// record in the chain.  Cyclic chains (e.g. swap a↔b) are left
// uncollapsed.

fn chain_collapse(actions: &mut Vec<Action>) {
    // Build src → index map for all renames.
    let mut src_to_idx: HashMap<String, usize> = HashMap::new();
    for (i, action) in actions.iter().enumerate() {
        if let Action::Rename { old, .. } | Action::Replace { old, .. } = action {
            src_to_idx.insert(old.clone(), i);
        }
    }

    let mut to_remove: HashSet<usize> = HashSet::new();
    let mut processed: HashSet<usize> = HashSet::new();

    // Two passes: first from chain heads (src not a dest), then remaining.
    // The difference is whether already-processed indices are skipped
    // during chain following (second pass skips them).
    for pass in 0..2 {
        let skip_processed = pass == 1;

        // On first pass, build dest set to identify chain heads.
        let dests: HashSet<String> = if pass == 0 {
            actions
                .iter()
                .filter_map(|a| match a {
                    Action::Rename { new, .. } | Action::Replace { new, .. } => Some(new.clone()),
                    _ => None,
                })
                .collect()
        } else {
            HashSet::new()
        };

        for i in 0..actions.len() {
            if processed.contains(&i) || to_remove.contains(&i) {
                continue;
            }

            let src = match &actions[i] {
                Action::Rename { old, .. } | Action::Replace { old, .. } => old.clone(),
                _ => continue,
            };

            // First pass: only start from chain heads.
            if pass == 0 && dests.contains(&src) {
                continue;
            }

            // Follow chain forward in temporal order.
            let mut chain_indices = vec![i];
            let mut current_dest = match &actions[i] {
                Action::Rename { new, .. } | Action::Replace { new, .. } => new.clone(),
                _ => unreachable!(),
            };
            let mut visited: HashSet<String> = HashSet::new();
            visited.insert(src.clone());

            loop {
                if visited.contains(&current_dest) {
                    break;
                }
                if let Some(&next_idx) = src_to_idx.get(&current_dest) {
                    // Only follow forward in temporal order — the next
                    // rename must come after the current tail of the chain.
                    let tail_idx = *chain_indices.last().unwrap();
                    if next_idx <= tail_idx
                        || to_remove.contains(&next_idx)
                        || (skip_processed && processed.contains(&next_idx))
                    {
                        break;
                    }
                    visited.insert(current_dest.clone());
                    chain_indices.push(next_idx);
                    current_dest = match &actions[next_idx] {
                        Action::Rename { new, .. } | Action::Replace { new, .. } => new.clone(),
                        _ => unreachable!(),
                    };
                } else {
                    break;
                }
            }

            if chain_indices.len() <= 1 {
                processed.insert(i);
                continue;
            }

            // Check if the chain's final dest can reach back to the
            // chain head through the full rename graph (ignoring temporal
            // order).  If so, this is a cyclic swap — leave uncollapsed.
            let is_cycle = {
                let mut probe = current_dest.clone();
                let mut seen = HashSet::new();
                loop {
                    if probe == src {
                        break true;
                    }
                    if !seen.insert(probe.clone()) {
                        break false;
                    }
                    if let Some(&idx) = src_to_idx.get(&probe) {
                        probe = match &actions[idx] {
                            Action::Rename { new, .. } | Action::Replace { new, .. } => new.clone(),
                            _ => unreachable!(),
                        };
                    } else {
                        break false;
                    }
                }
            };

            if is_cycle {
                for &idx in &chain_indices {
                    processed.insert(idx);
                }
                continue;
            }

            // Collapse: head's dest becomes tail's dest, remove intermediates.
            let final_dest = current_dest;
            for &idx in &chain_indices[1..] {
                to_remove.insert(idx);
                processed.insert(idx);
            }

            match &mut actions[chain_indices[0]] {
                Action::Rename { new, .. } | Action::Replace { new, .. } => {
                    *new = final_dest;
                }
                _ => unreachable!(),
            }
            processed.insert(chain_indices[0]);
        }
    }

    remove_indices(actions, to_remove);
}

// ── Pass 3: Cancel ───────────────────────────────────────────────────
//
// A(x, ino) + D(x) → removed.
// M(x, ino) + D(x) → D(x).

fn cancel(actions: &mut Vec<Action>) {
    let mut add_at: HashMap<String, usize> = HashMap::new();
    let mut to_remove: HashSet<usize> = HashSet::new();

    for i in 0..actions.len() {
        match &actions[i] {
            Action::Add { path, .. } => {
                add_at.insert(path.clone(), i);
            }
            Action::Modify { path, .. } => {
                add_at.insert(path.clone(), i);
            }
            Action::Delete { path } => {
                if let Some(add_idx) = add_at.remove(path) {
                    let is_modify = matches!(&actions[add_idx], Action::Modify { .. });
                    if is_modify {
                        // M(x) + D(x) → D(x) — remove M, keep D
                        to_remove.insert(add_idx);
                    } else {
                        // A(x) + D(x) → removed — remove both
                        to_remove.insert(add_idx);
                        to_remove.insert(i);
                    }
                }
            }
            Action::Rename { old, new, .. } | Action::Replace { old, new, .. } => {
                add_at.remove(old);
                add_at.remove(new);
            }
        }
    }

    remove_indices(actions, to_remove);
}

// ── Pass 4: Merge modifies ──────────────────────────────────────────
//
// M(x, ino=1) + M(x, ino=2) → M(x, ino=2).

fn merge_modifies(actions: &mut Vec<Action>) {
    let mut modify_at: HashMap<String, usize> = HashMap::new();
    let mut to_remove: HashSet<usize> = HashSet::new();

    for (i, action) in actions.iter().enumerate() {
        if let Action::Modify { path, .. } = action
            && let Some(prev_idx) = modify_at.insert(path.clone(), i)
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
    let mut i = 0;
    actions.retain(|_| {
        let keep = !to_remove.contains(&i);
        i += 1;
        keep
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Chain collapse ───────────────────────────────────────────────

    #[test]
    fn simple_chain_collapses() {
        let records = vec![
            Record::Redirect {
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
        let al = simplify(records);
        assert_eq!(al.0.len(), 1);
        assert!(matches!(&al.0[0], Action::Rename { old, new, .. } if old == "/a" && new == "/c"));
    }

    #[test]
    fn three_step_chain_collapses() {
        let records = vec![
            Record::Redirect {
                old: "/a".into(),
                new: "/b".into(),
                dtype: Some(DType::File),
            },
            Record::Redirect {
                old: "/b".into(),
                new: "/c".into(),
                dtype: Some(DType::File),
            },
            Record::Redirect {
                old: "/c".into(),
                new: "/d".into(),
                dtype: Some(DType::File),
            },
        ];
        let al = simplify(records);
        assert_eq!(al.0.len(), 1);
        assert!(matches!(&al.0[0], Action::Rename { old, new, .. } if old == "/a" && new == "/d"));
    }

    #[test]
    fn two_cycle_swap_not_collapsed() {
        let records = vec![
            Record::Redirect {
                old: "/a".into(),
                new: "/tmp".into(),
                dtype: Some(DType::File),
            },
            Record::Redirect {
                old: "/b".into(),
                new: "/a".into(),
                dtype: Some(DType::File),
            },
            Record::Redirect {
                old: "/tmp".into(),
                new: "/b".into(),
                dtype: Some(DType::File),
            },
        ];
        let al = simplify(records);
        // All 3 renames remain (cycle: a→tmp→b and b→a→tmp forms a cycle)
        assert_eq!(
            al.0.len(),
            3,
            "cyclic swap should not collapse, got: {:?}",
            al.0
        );
    }

    #[test]
    fn three_cycle_rotation_not_collapsed() {
        let records = vec![
            Record::Redirect {
                old: "/a".into(),
                new: "/tmp".into(),
                dtype: Some(DType::File),
            },
            Record::Redirect {
                old: "/b".into(),
                new: "/a".into(),
                dtype: Some(DType::File),
            },
            Record::Redirect {
                old: "/c".into(),
                new: "/b".into(),
                dtype: Some(DType::File),
            },
            Record::Redirect {
                old: "/tmp".into(),
                new: "/c".into(),
                dtype: Some(DType::File),
            },
        ];
        let al = simplify(records);
        assert_eq!(
            al.0.len(),
            4,
            "3-cycle rotation should not collapse, got: {:?}",
            al.0
        );
    }

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
        let al = simplify(records);
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
        let al = simplify(records);
        assert_eq!(al.0.len(), 1);
        assert!(matches!(&al.0[0], Action::Delete { path } if path == "/x"));
    }

    // ── Staged rename cancel ─────────────────────────────────────────

    #[test]
    fn staged_rename_cancels_old_add() {
        // touch x (staged), mv x y → kernel emits: A(x,1) + D(x) + A(y,1)
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
        let al = simplify(records);
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
        let al = simplify(records);
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
        let al = simplify(records);
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
        let al = simplify(records);
        assert_eq!(al.0.len(), 2, "expected D(a) + M(b,5), got: {:?}", al.0);
        assert!(matches!(&al.0[0], Action::Delete { path } if path == "/a"));
        assert!(
            matches!(&al.0[1], Action::Modify { path, ino: 5, .. } if path == "/b"),
            "P+M should decompose to D+M, got: {:?}",
            al.0[1]
        );
    }

    // ── Interleaved chains ──────────────────────────────────────────

    #[test]
    fn independent_renames_not_collapsed() {
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
        let al = simplify(records);
        assert_eq!(al.0.len(), 2);
    }

    // ── Empty input ─────────────────────────────────────────────────

    #[test]
    fn simplify_empty() {
        let al = simplify(vec![]);
        assert!(al.0.is_empty());
    }

    // ── K/S records filtered ────────────────────────────────────────

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
        let al = simplify(records);
        assert_eq!(al.0.len(), 1);
        assert!(matches!(&al.0[0], Action::Add { path, .. } if path == "/a"));
    }

    // ── Chain collapse preserves dtype ──────────────────────────────

    #[test]
    fn chain_collapse_preserves_head_dtype() {
        let records = vec![
            Record::Redirect {
                old: "/a".into(),
                new: "/b".into(),
                dtype: Some(DType::Dir),
            },
            Record::Redirect {
                old: "/b".into(),
                new: "/c".into(),
                dtype: Some(DType::Dir),
            },
        ];
        let al = simplify(records);
        assert_eq!(al.0.len(), 1);
        assert!(
            matches!(&al.0[0], Action::Rename { old, new, dtype: DType::Dir } if old == "/a" && new == "/c"),
            "dtype should be Dir, got: {:?}",
            al.0[0]
        );
    }

    // ── Temporal ordering in chain collapse ────────────────────────

    #[test]
    fn backward_chain_not_collapsed() {
        // R(b,c) at i=0 then R(a,b) at i=1 — b→c happened before a→b
        // exists, so they must NOT collapse.
        let records = vec![
            Record::Redirect {
                old: "/b".into(),
                new: "/c".into(),
                dtype: Some(DType::File),
            },
            Record::Redirect {
                old: "/a".into(),
                new: "/b".into(),
                dtype: Some(DType::File),
            },
        ];
        let al = simplify(records);
        assert_eq!(
            al.0.len(),
            2,
            "backward chain must not collapse, got: {:?}",
            al.0
        );
    }

    #[test]
    fn three_element_backward_chain_not_collapsed() {
        // c→d at i=0, b→c at i=1, a→b at i=2: all backward, none should collapse.
        let records = vec![
            Record::Redirect {
                old: "/c".into(),
                new: "/d".into(),
                dtype: Some(DType::File),
            },
            Record::Redirect {
                old: "/b".into(),
                new: "/c".into(),
                dtype: Some(DType::File),
            },
            Record::Redirect {
                old: "/a".into(),
                new: "/b".into(),
                dtype: Some(DType::File),
            },
        ];
        let al = simplify(records);
        assert_eq!(
            al.0.len(),
            3,
            "all backward chains should remain, got: {:?}",
            al.0
        );
    }

    #[test]
    fn mixed_forward_backward_chain_partial_collapse() {
        // a→b at i=0 (forward), c→d at i=1 (independent), b→c at i=2
        // a→b + b→c should collapse to a→c, c→d stays.
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
            Record::Redirect {
                old: "/b".into(),
                new: "/c".into(),
                dtype: Some(DType::File),
            },
        ];
        let al = simplify(records);
        assert_eq!(
            al.0.len(),
            2,
            "a→b + b→c should collapse, c→d stays, got: {:?}",
            al.0
        );
    }

    // ── Replace chain collapse ──────────────────────────────────────

    #[test]
    fn replace_chain_collapses() {
        let records = vec![
            Record::Replace {
                old: "/a".into(),
                new: "/b".into(),
                dtype: Some(DType::File),
            },
            Record::Replace {
                old: "/b".into(),
                new: "/c".into(),
                dtype: Some(DType::File),
            },
        ];
        let al = simplify(records);
        assert_eq!(al.0.len(), 1, "P(a,b)+P(b,c) should collapse, got: {:?}", al.0);
        assert!(
            matches!(&al.0[0], Action::Replace { old, new, .. } if old == "/a" && new == "/c"),
            "expected P(a,c), got: {:?}",
            al.0[0]
        );
    }

    #[test]
    fn mixed_rename_replace_chain_collapses() {
        // R(a,b) + P(b,c): head is Rename, tail is Replace.
        // Chain should collapse; head keeps its type (Rename).
        let records = vec![
            Record::Redirect {
                old: "/a".into(),
                new: "/b".into(),
                dtype: Some(DType::File),
            },
            Record::Replace {
                old: "/b".into(),
                new: "/c".into(),
                dtype: Some(DType::File),
            },
        ];
        let al = simplify(records);
        assert_eq!(
            al.0.len(),
            1,
            "R(a,b)+P(b,c) should collapse, got: {:?}",
            al.0
        );
        assert!(
            matches!(&al.0[0], Action::Rename { old, new, .. } if old == "/a" && new == "/c"),
            "head type (Rename) preserved, got: {:?}",
            al.0[0]
        );
    }

    #[test]
    fn mixed_replace_rename_chain_collapses() {
        // P(a,b) + R(b,c): head is Replace, tail is Rename.
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
        let al = simplify(records);
        assert_eq!(
            al.0.len(),
            1,
            "P(a,b)+R(b,c) should collapse, got: {:?}",
            al.0
        );
        assert!(
            matches!(&al.0[0], Action::Replace { old, new, .. } if old == "/a" && new == "/c"),
            "head type (Replace) preserved, got: {:?}",
            al.0[0]
        );
    }

    // ── Decompose cancellation on intervening action ────────────────

    #[test]
    fn intervening_delete_prevents_decompose() {
        // R(a,b) + D(b) + M(b,5): the Delete at /b should cancel
        // the rename-dest tracking so M(b) is NOT decomposed with R(a,b).
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
        let al = simplify(records);
        // R(a,b) stays as Rename, D(b) cancels with M(b), leaving just the rename.
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
        let al = simplify(records);
        assert_eq!(al.0.len(), 1);
        assert!(matches!(&al.0[0], Action::Replace { old, new, .. } if old == "/a" && new == "/b"));
    }

    // ── Mixed Replace + Redirect chain ──────────────────────────────

    #[test]
    fn replace_then_redirect_chain_collapses() {
        // P(a→b) + R(b→c) → P(a→c): head keeps Replace type
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
        let al = simplify(records);
        assert_eq!(
            al.0.len(),
            1,
            "P+R chain should collapse, got: {:?}",
            al.0
        );
        assert!(
            matches!(&al.0[0], Action::Replace { old, new, .. } if old == "/a" && new == "/c"),
            "expected P(a,c), got: {:?}",
            al.0[0]
        );
    }

    // ── Multi-segment resolution (via SegmentedJournal pipeline) ─────
    //
    // These verify that simplify().collapse() produces correct results
    // when records span checkpoint boundaries and are filtered by liveness.

    use super::super::segment::SegmentedJournal;
    use super::super::types::RawJournal;

    /// Helper: run the full pipeline on raw records including K/S markers.
    fn resolve_all(records: Vec<Record>) -> Vec<(String, super::super::types::Change)> {
        let sj = SegmentedJournal::new(RawJournal(records));
        let live = sj.live().into_records();
        let al = simplify(live);
        al.collapse().0
    }

    #[test]
    fn segments_add_delete_readd_across_checkpoints() {
        // Seg0: A(x) | K1 | Seg1: D(x) | K2 | Seg2: A(x, new ino)
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
        // Seg0: M(x, ino=1) | K1 | Seg1: M(x, ino=2)
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
        // Seg0: M(x, ino=1) | K1 | Seg1: D(x)
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
        // Seg0: R(a, b) | K1 | Seg1: M(b, ino=5)
        // After decompose: R(a,b) + M(b,5) → D(a) + A(b,5)
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
    fn segments_redirect_rename_across_checkpoints() {
        // Seg0: R(a, b) | K1 | Seg1: R(b, c) — chain should collapse to R(a, c)
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
            Record::Redirect {
                old: "/b".into(),
                new: "/c".into(),
                dtype: Some(DType::File),
            },
        ];
        let cs = resolve_all(records);
        let has_renamed = cs.iter().any(|(p, c)| {
            p == "/c"
                && matches!(c, super::super::types::Change::Renamed { from, .. } if from == "/a")
        });
        assert!(
            has_renamed,
            "expected Renamed(a→c) after cross-checkpoint chain, got: {cs:?}",
        );
    }

    #[test]
    fn segments_restore_kills_dead_segment() {
        // Seg0: A(x) | K1 | Seg1: A(y) | K2 | S3(target=K1) | Seg3: A(z)
        // Restore to K1 means Seg1 is dead.
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
        // Seg0: D(x) | K1 | Seg1: A(x, ino=2)
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

    #[test]
    fn segments_preserve_dtype_across_checkpoints() {
        // Seg0: R(a, b, Dir) | K1 | Seg1: R(b, c, Dir)
        // Chain collapses to R(a, c, Dir).
        let records = vec![
            Record::Redirect {
                old: "/a".into(),
                new: "/b".into(),
                dtype: Some(DType::Dir),
            },
            Record::Checkpoint {
                gen_id: 1,
                name: "k1".into(),
            },
            Record::Redirect {
                old: "/b".into(),
                new: "/c".into(),
                dtype: Some(DType::Dir),
            },
        ];
        let cs = resolve_all(records);
        let has_dir_rename = cs.iter().any(|(p, c)| {
            p == "/c"
                && matches!(c, super::super::types::Change::Renamed { from, dtype: DType::Dir, .. } if from == "/a")
        });
        assert!(
            has_dir_rename,
            "expected Renamed(a→c, Dir) with dtype preserved, got: {cs:?}",
        );
    }

    #[test]
    fn replace_then_rename_chain_tracks_overwrites() {
        // P(b→a, overwrites base 'a') then R(a→c): simplify chain-collapses
        // to P(b→c), then collapse produces Replaced(b→c) + Deleted(/b).
        let records = vec![
            Record::Replace {
                old: "/b".into(),
                new: "/a".into(),
                dtype: Some(DType::File),
            },
            Record::Redirect {
                old: "/a".into(),
                new: "/c".into(),
                dtype: Some(DType::File),
            },
        ];
        let cs = resolve_all(records);
        let has_replaced = cs.iter().any(|(p, c)| {
            matches!(c, super::super::types::Change::Replaced { from, .. } if from == "/b" && p == "/c")
        });
        let has_del_b = cs
            .iter()
            .any(|(p, c)| p == "/b" && matches!(c, super::super::types::Change::Deleted));
        assert!(has_replaced, "expected Replaced(b→c), got: {cs:?}");
        assert!(has_del_b, "expected Deleted(/b) as source, got: {cs:?}");
    }
}

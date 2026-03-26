// agfs CLI — journal/plan.rs
//
// DirTree → Actions: convert the collapsed overlay state back into a
// minimal ordered list of filesystem mutations for commit.
//
// Implements `DirTree::into_plan()` — the inverse of `DirTree::build()`.
//
// ## Algorithm
//
// 1. **Collect**: DFS the tree, partitioning nodes into three buckets:
//      - stages  (StagedFile)  — copy a staged inode to base
//      - renames (BasePath)    — move a base path to a new location
//      - deletes (Tombstone)   — remove a base path
//    Passthrough nodes emit no action; they exist only as scaffolding.
//    Recursion stops at Tombstone: a tombstoned dir is deleted whole
//    (`remove_dir_all`), so child actions are unnecessary.  Any children
//    staged before the delete are dead — the Tombstone overwrites them.
//
// 2. **Order renames** via a two-phase save/place strategy.  A rename
//    reads from `src` (a base path) and writes to `dst`.  Sources can
//    overlap (a parent and a child may both be sources), and a rename's
//    destination may overwrite another rename's source.  Rather than
//    building a dependency graph and breaking cycles, we sidestep the
//    problem entirely:
//
//    **Phase 1 — save**: move every source to a temp path, deepest first.
//    This extracts children before parents, so moving a parent directory
//    doesn't carry away an already-saved child.
//
//    **Phase 2 — place**: move each temp to its destination, in DFS order
//    (parent destinations before children).  This ensures parent dirs
//    exist before children are placed inside them.
//
//    Cost: 2n rename syscalls instead of n.  Since `fs::rename` is
//    metadata-only, this is negligible for typical commit sizes.
//
// 3. **Concatenate**: save-renames → place-renames → deletes → stages.
//
// ## Why this order is correct
//
// - **Save-renames first**: all sources are safely moved to temp paths
//   before any destination is written.  No rename can invalidate another's
//   source.
// - **Place-renames next**: destinations are written in DFS order (parent
//   before child).  `apply_rename`'s `remove_existing` at each destination
//   can't wipe a child that hasn't been placed yet.
// - **Deletes after renames**: renames read from base paths; deletes destroy
//   base paths.  By this point all sources have been saved.
// - **Stages last**: parent directories exist — either from base, renames,
//   or earlier stages.  DFS pre-order guarantees parents staged before
//   children.

use super::tree::DirTree;
use super::types::*;

/// A commit plan: the minimal set of filesystem mutations to apply.
///
/// Renames are split into two phases (save sources, then place at
/// destinations).  Iterating yields all actions in execution order.
pub struct CommitPlan {
    /// Phase 1: save rename sources to temp paths (deepest source first).
    pub saves: Vec<Action>,
    /// Phase 2: place temp paths at destinations (DFS order, parent first).
    pub places: Vec<Action>,
    /// Phase 3: tombstone removals.
    pub deletes: Vec<Action>,
    /// Phase 4: staged inode copies (DFS pre-order, parent first).
    pub stages: Vec<Action>,
}

impl CommitPlan {
    pub fn is_empty(&self) -> bool {
        self.saves.is_empty() && self.deletes.is_empty() && self.stages.is_empty()
    }

    pub fn len(&self) -> usize {
        // saves and places are paired — count as one rename each
        self.saves.len() + self.deletes.len() + self.stages.len()
    }
}

/// Convert a DirTree into a commit plan.
pub(super) fn into_plan(tree: &DirTree) -> CommitPlan {
    let mut renames = Vec::new();
    let mut deletes = Vec::new();
    let mut stages = Vec::new();
    let mut prefix = String::new();
    collect(tree, &mut prefix, &mut renames, &mut deletes, &mut stages);

    let (saves, places) = order_renames(renames);

    CommitPlan {
        saves,
        places,
        deletes,
        stages,
    }
}

fn collect(
    tree: &DirTree,
    prefix: &mut String,
    renames: &mut Vec<Action>,
    deletes: &mut Vec<Action>,
    stages: &mut Vec<Action>,
) {
    for (name, node) in &tree.nodes {
        let path_len = prefix.len();
        prefix.push('/');
        prefix.push_str(name);

        match &node.target {
            Target::StagedFile(ino) => {
                stages.push(Action::Stage { path: prefix.clone(), ino: *ino });
            }
            Target::BasePath(src) => {
                renames.push(Action::Rename {
                    dst: prefix.clone(),
                    src: src.clone(),
                });
            }
            Target::Tombstone => {
                deletes.push(Action::Delete { path: prefix.clone() });
            }
            Target::Passthrough => {}
        }

        if !matches!(node.target, Target::Tombstone) {
            collect(&node.children, prefix, renames, deletes, stages);
        }

        prefix.truncate(path_len);
    }
}


// ── Rename ordering (two-phase save/place) ────────────────────────────

/// Split renames into save and place phases.
///
/// - **saves**: move each source to a temp path, deepest source first.
/// - **places**: move each temp to its destination, in DFS order (as collected).
fn order_renames(renames: Vec<Action>) -> (Vec<Action>, Vec<Action>) {
    if renames.is_empty() {
        return (Vec::new(), Vec::new());
    }

    // Sort by source depth (deepest first) for the save phase.
    let mut indexed: Vec<(usize, Action)> = renames.into_iter().enumerate().collect();
    indexed.sort_by(|a, b| {
        let src_a = match &a.1 { Action::Rename { src, .. } => src, _ => unreachable!() };
        let src_b = match &b.1 { Action::Rename { src, .. } => src, _ => unreachable!() };
        src_b.len().cmp(&src_a.len())
    });

    let mut saves = Vec::with_capacity(indexed.len());
    let mut place_by_idx: Vec<Option<Action>> = vec![None; indexed.len()];

    for (n, (orig_idx, action)) in indexed.into_iter().enumerate() {
        let (dst, src) = match action {
            Action::Rename { dst, src } => (dst, src),
            _ => unreachable!(),
        };

        let tmp = temp_path(n);
        saves.push(Action::Rename { dst: tmp.clone(), src });
        place_by_idx[orig_idx] = Some(Action::Rename { dst, src: tmp });
    }

    // Places are in original DFS order (parent destinations before children).
    let places = place_by_idx.into_iter().map(|a| a.unwrap()).collect();
    (saves, places)
}

fn temp_path(n: usize) -> String {
    format!("/.agfs-commit-tmp-{}", n)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build(actions: &[Action]) -> DirTree {
        DirTree::build(std::iter::once(Segment {
            from: 0,
            records: actions.to_vec(),
        }))
    }

    fn add(path: &str, ino: u32) -> Action {
        Action::Stage {
            path: path.into(),
            ino,
        }
    }

    fn delete(path: &str) -> Action {
        Action::Delete {
            path: path.into(),
        }
    }

    fn rename(dest: &str, src: &str) -> Action {
        Action::Rename {
            dst: dest.into(),
            src: src.into(),
        }
    }

    fn get_renames(plan: &CommitPlan) -> Vec<(String, String)> {
        // Places have the final (dst, temp_src).  Map back to (dst, original_src)
        // by pairing with saves which have (temp_dst, original_src).
        plan.places
            .iter()
            .map(|op| {
                let Action::Rename { dst, src: tmp } = op else { unreachable!() };
                // Find the save that wrote to this temp path.
                let orig_src = plan.saves.iter().find_map(|s| {
                    let Action::Rename { dst: save_dst, src: save_src } = s else { unreachable!() };
                    if save_dst == tmp { Some(save_src.clone()) } else { None }
                }).unwrap_or_else(|| tmp.clone());
                (dst.clone(), orig_src)
            })
            .collect()
    }

    fn get_stages(plan: &CommitPlan) -> Vec<(String, u32)> {
        plan.stages
            .iter()
            .map(|op| match op {
                Action::Stage { path, ino } => (path.clone(), *ino),
                _ => unreachable!(),
            })
            .collect()
    }

    fn get_deletes(plan: &CommitPlan) -> Vec<String> {
        plan.deletes
            .iter()
            .map(|op| match op {
                Action::Delete { path } => path.clone(),
                _ => unreachable!(),
            })
            .collect()
    }

    // ── CommitPlan basics ────────────────────────────────────────────────

    #[test]
    fn empty_tree_produces_empty_plan() {
        let plan = build(&[]).into_plan();
        assert!(plan.is_empty());
    }

    #[test]
    fn non_empty_plan_is_not_empty() {
        let plan = build(&[add("/a", 1)]).into_plan();
        assert!(!plan.is_empty());
    }

    #[test]
    fn staged_then_deleted_plan_is_not_empty() {
        let plan = build(&[add("/a", 1), delete("/a")]).into_plan();
        assert!(!plan.is_empty(), "tombstone should still be non-empty");
    }

    // ── Plan generation: basic ────────────────────────────────────────

    #[test]
    fn plan_single_stage() {
        let plan = build(&[add("/a", 1)]).into_plan();
        assert!(get_renames(&plan).is_empty());
        assert!(get_deletes(&plan).is_empty());
        assert_eq!(get_stages(&plan), vec![("/a".to_string(), 1)]);
    }

    #[test]
    fn plan_single_delete() {
        let plan = build(&[delete("/a")]).into_plan();
        assert!(get_renames(&plan).is_empty());
        assert_eq!(get_deletes(&plan), vec!["/a"]);
        assert!(get_stages(&plan).is_empty());
    }

    #[test]
    fn plan_single_rename() {
        let plan = build(&[rename("/b", "/a")]).into_plan();
        assert_eq!(
            get_renames(&plan),
            vec![("/b".to_string(), "/a".to_string())]
        );
        assert_eq!(get_deletes(&plan), vec!["/a"]);
        assert!(get_stages(&plan).is_empty());
    }

    #[test]
    fn plan_stage_then_delete_collapses() {
        let plan = build(&[add("/a", 1), delete("/a")]).into_plan();
        assert!(get_renames(&plan).is_empty());
        assert_eq!(get_deletes(&plan), vec!["/a"]);
        assert!(get_stages(&plan).is_empty());
    }

    #[test]
    fn plan_stage_overwrite_collapses() {
        let plan = build(&[add("/a", 1), add("/a", 2)]).into_plan();
        assert!(get_renames(&plan).is_empty());
        assert!(get_deletes(&plan).is_empty());
        assert_eq!(get_stages(&plan), vec![("/a".to_string(), 2)]);
    }

    #[test]
    fn plan_mixed_ops() {
        let plan = build(&[add("/a", 1), rename("/c", "/b"), delete("/d")]).into_plan();
        assert_eq!(
            get_renames(&plan),
            vec![("/c".to_string(), "/b".to_string())]
        );
        assert!(get_deletes(&plan).contains(&"/b".to_string()));
        assert!(get_deletes(&plan).contains(&"/d".to_string()));
        assert_eq!(get_stages(&plan), vec![("/a".to_string(), 1)]);
    }

    // ── Tombstoned dir ────────────────────────────────────────────────

    #[test]
    fn plan_tombstoned_dir_skips_subtree() {
        let plan = build(&[add("/dir/f", 1), delete("/dir/f"), delete("/dir")]).into_plan();
        assert!(get_renames(&plan).is_empty());
        assert!(get_stages(&plan).is_empty());
        assert_eq!(get_deletes(&plan), vec!["/dir"]);
    }

    // ── DFS ordering ───────────────────────────────────────────────────

    #[test]
    fn stages_parent_before_child() {
        let plan = build(&[add("/a/b/c", 3), add("/a", 1), add("/a/b", 2)]).into_plan();
        assert_eq!(
            get_stages(&plan),
            vec![
                ("/a".to_string(), 1),
                ("/a/b".to_string(), 2),
                ("/a/b/c".to_string(), 3),
            ]
        );
    }

    // ── Two-phase rename ordering ───────────────────────────────────────

    #[test]
    fn saves_deepest_source_first() {
        // Sources /dir and /dir/f: save /dir/f first (deeper).
        let plan = build(&[
            rename("/x", "/dir/file"),
            rename("/y", "/dir"),
        ]).into_plan();
        assert_eq!(plan.saves.len(), 2);
        // First save should be the deeper source.
        let first_src = match &plan.saves[0] {
            Action::Rename { src, .. } => src.clone(),
            _ => unreachable!(),
        };
        assert_eq!(first_src, "/dir/file", "deeper source must be saved first");
    }

    #[test]
    fn places_preserve_dfs_order() {
        // Three renames with nested destinations: /a, /a/b, /a/b/c.
        // DFS gives parent before child, places must preserve this.
        let plan = build(&[
            rename("/a", "/x"),
            rename("/a/b", "/y"),
            rename("/a/b/c", "/z"),
        ]).into_plan();
        let renames = get_renames(&plan);
        let idx_a = renames.iter().position(|p| p.0 == "/a").unwrap();
        let idx_ab = renames.iter().position(|p| p.0 == "/a/b").unwrap();
        let idx_abc = renames.iter().position(|p| p.0 == "/a/b/c").unwrap();
        assert!(idx_a < idx_ab, "/a must come before /a/b");
        assert!(idx_ab < idx_abc, "/a/b must come before /a/b/c");
    }

    #[test]
    fn saves_and_places_paired() {
        // Every rename produces one save + one place.
        let plan = build(&[rename("/a", "/c"), rename("/c", "/b")]).into_plan();
        assert_eq!(plan.saves.len(), plan.places.len());
        assert_eq!(plan.saves.len(), 2);
    }

    #[test]
    fn source_resolution_in_chain() {
        // mv /a /b, mv /b /c, then mv /c/f /x.
        // Chain collapse: /c ← /a. Source resolution: /x ← /a/f.
        let plan = build(&[
            rename("/b", "/a"),
            rename("/c", "/b"),
            rename("/x", "/c/f"),
        ]).into_plan();
        let renames = get_renames(&plan);
        // /x should have source /a/f (resolved through chain).
        let x_rename = renames.iter().find(|(dst, _)| dst == "/x").unwrap();
        assert_eq!(x_rename.1, "/a/f", "source must be resolved to base path");
    }

    #[test]
    fn redirect_child_rename() {
        // mv /dir /other, then mv /dir/f /other/renamed.
        let plan = build(&[
            rename("/other", "/dir"),
            rename("/other/renamed", "/dir/f"),
        ]).into_plan();
        let renames = get_renames(&plan);
        assert_eq!(renames.len(), 2);
    }

    // ── Swap / rotation ─────────────────────────────────────────────────

    #[test]
    fn swap_produces_correct_logical_renames() {
        let plan = build(&[
            rename("/tmp", "/a"),
            rename("/a", "/b"),
            rename("/b", "/tmp"),
        ]).into_plan();
        let renames = get_renames(&plan);
        // After chain collapse: /a ← /b, /b ← /a (swap).
        assert_eq!(renames.len(), 2, "swap should produce 2 logical renames");
    }

    #[test]
    fn three_way_rotation_produces_correct_renames() {
        let plan = build(&[
            rename("/tmp", "/a"),
            rename("/a", "/c"),
            rename("/c", "/b"),
            rename("/b", "/tmp"),
        ]).into_plan();
        let renames = get_renames(&plan);
        assert_eq!(renames.len(), 3, "3-way rotation should produce 3 logical renames");
    }

    // ── Idempotence: actions → tree → actions → tree ──────────────────
    //
    // The DirTree is a fixed point: building a tree from its own emitted
    // actions must produce an identical tree. This is the "idempotence"
    // (or "projection") property: project ∘ project = project.
    //
    // Cycle-breaking temp renames are excluded — they are apply-time
    // artifacts that don't represent logical state changes.

    fn actions_without_temps(plan: &CommitPlan) -> Vec<Action> {
        // Extract logical (dst, original_src) renames from the places,
        // resolving through the save mapping.
        get_renames(plan)
            .into_iter()
            .map(|(dst, src)| Action::Rename { dst, src })
            .chain(plan.deletes.iter().cloned())
            .chain(plan.stages.iter().cloned())
            .collect()
    }

    /// For cycle cases, we can't get exact idempotence because temp renames
    /// rewrite sources. Instead, verify the trees agree on all non-passthrough
    /// entries (same paths map to same targets).
    fn assert_same_entries(tree1: &DirTree, tree2: &DirTree) {
        let mut entries1 = Vec::new();
        tree1.for_each(|p, t| entries1.push((p.to_string(), t.clone())));
        entries1.sort_by(|a, b| a.0.cmp(&b.0));

        let mut entries2 = Vec::new();
        tree2.for_each(|p, t| entries2.push((p.to_string(), t.clone())));
        entries2.sort_by(|a, b| a.0.cmp(&b.0));

        // Filter out temp path artifacts
        let entries1: Vec<_> = entries1
            .into_iter()
            .filter(|(p, _)| !p.contains(".agfs-commit-tmp-"))
            .collect();
        let entries2: Vec<_> = entries2
            .into_iter()
            .filter(|(p, _)| !p.contains(".agfs-commit-tmp-"))
            .collect();

        assert_eq!(entries1, entries2, "trees should have same logical entries");
    }

    fn assert_idempotent(input: &[Action]) {
        let tree1 = build(input);
        let plan = tree1.into_plan();
        let logical = actions_without_temps(&plan);
        let tree2 = build(&logical);
        assert_eq!(tree1, tree2, "tree should be a fixed point of build ∘ into_plan");
    }

    #[test]
    fn idempotent_single_stage() {
        assert_idempotent(&[add("/a", 1)]);
    }

    #[test]
    fn idempotent_single_delete() {
        assert_idempotent(&[delete("/a")]);
    }

    #[test]
    fn idempotent_single_rename() {
        assert_idempotent(&[rename("/b", "/a")]);
    }

    #[test]
    fn idempotent_stage_overwrite() {
        assert_idempotent(&[add("/a", 1), add("/a", 2)]);
    }

    #[test]
    fn idempotent_stage_then_delete() {
        assert_idempotent(&[add("/a", 1), delete("/a")]);
    }

    #[test]
    fn idempotent_rename_chain() {
        assert_idempotent(&[rename("/b", "/a"), rename("/c", "/b")]);
    }

    #[test]
    fn idempotent_mixed_ops() {
        assert_idempotent(&[
            add("/x", 1),
            rename("/c", "/b"),
            delete("/d"),
            add("/dir", 2),
            add("/dir/f", 3),
        ]);
    }

    #[test]
    fn idempotent_dir_rename_with_children() {
        assert_idempotent(&[
            add("/dir", 1),
            add("/dir/f1", 2),
            add("/dir/f2", 3),
            rename("/other", "/dir"),
        ]);
    }

    #[test]
    fn swap_logical_renames_correct() {
        // Swap: a↔b.  The two-phase approach handles this without cycle
        // detection — both sources are saved to temps before any destination
        // is written.  Verify the logical renames are correct.
        let plan = build(&[
            rename("/tmp", "/a"),
            rename("/a", "/b"),
            rename("/b", "/tmp"),
        ]).into_plan();
        let mut renames = get_renames(&plan);
        renames.sort();
        assert_eq!(
            renames,
            vec![("/a".to_string(), "/b".to_string()), ("/b".to_string(), "/a".to_string())]
        );
    }

    #[test]
    fn idempotent_nested_stages() {
        assert_idempotent(&[
            add("/a", 1),
            add("/a/b", 2),
            add("/a/b/c", 3),
        ]);
    }

    #[test]
    fn idempotent_delete_dir() {
        // Tombstoned dirs may have residual children in the tree that
        // get dropped by into_plan (it doesn't recurse into tombstoned
        // dirs). Verify the round-tripped tree is a subset with matching
        // targets.
        let tree1 = build(&[
            add("/dir/f", 1),
            delete("/dir/f"),
            delete("/dir"),
        ]);
        let plan = tree1.into_plan();
        let all = actions_without_temps(&plan);
        let tree2 = build(&all);

        let mut entries2 = Vec::new();
        tree2.for_each(|p, t| entries2.push((p.to_string(), t.clone())));
        for (path, target) in &entries2 {
            assert_eq!(
                tree1.get(path), Some(target),
                "tree2 entry {path} should match tree1"
            );
        }
    }
}

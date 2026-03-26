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
// 2. **Order renames** via selective two-phase save/place.  A rename
//    reads from `src` (a base path) and writes to `dst`.  Two renames
//    *conflict* when any path (src or dst) of one is an ancestor,
//    descendant, or equal to any path of the other.  Conflicts are
//    detected in O(n log n) by sorting all 2n paths in trie order and
//    sweeping with a stack of active ancestors.
//
//    **Independent renames** (no conflict with any other rename) skip
//    the temp-path detour and execute as a single rename syscall, in
//    DFS order of their destinations.
//
//    **Conflicted renames** use a two-phase strategy:
//
//    Phase 1 — save: move every conflicted source to a temp path,
//    deepest first.  This extracts children before parents, so moving
//    a parent directory doesn't carry away an already-saved child.
//
//    Phase 2 — place: move each temp to its destination, in DFS order
//    (parent destinations before children).  This ensures parent dirs
//    exist before children are placed inside them.
//
//    Cost: n + k rename syscalls where k is the number of conflicted
//    renames (each costs 2).  Best case n (all independent), worst
//    case 2n (all conflicted).
//
// 3. **Concatenate**: save-renames → direct-renames → place-renames
//    → deletes → stages.
//
// ## Why this order is correct
//
// - **Save-renames first**: all conflicted sources are safely moved to
//   temp paths before any destination is written.
// - **Direct-renames next**: independent renames execute as single
//   syscalls.  They have no path overlap with any other rename, so
//   order among themselves only requires parent-before-child (DFS).
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
/// Renames are split into direct (conflict-free) and two-phase
/// (save sources, then place at destinations).  Use `actions()` to
/// iterate all mutations in the correct execution order.
pub struct CommitPlan {
    /// Phase 1: save conflicted rename sources to temp paths (deepest source first).
    saves: Vec<Action>,
    /// Phase 2: direct renames — conflict-free, single rename syscall (DFS order).
    directs: Vec<Action>,
    /// Phase 3: place temp paths at destinations (DFS order, parent first).
    places: Vec<Action>,
    /// Phase 4: tombstone removals.
    deletes: Vec<Action>,
    /// Phase 5: staged inode copies (DFS pre-order, parent first).
    stages: Vec<Action>,
}

impl CommitPlan {
    pub fn is_empty(&self) -> bool {
        self.saves.is_empty()
            && self.directs.is_empty()
            && self.deletes.is_empty()
            && self.stages.is_empty()
    }

    pub fn len(&self) -> usize {
        // saves and places are paired — count as one rename each
        self.saves.len() + self.directs.len() + self.deletes.len() + self.stages.len()
    }

    /// Iterate all actions in execution order.
    pub fn iter(&self) -> impl Iterator<Item = &Action> {
        self.saves
            .iter()
            .chain(&self.directs)
            .chain(&self.places)
            .chain(&self.deletes)
            .chain(&self.stages)
    }
}

/// Convert a DirTree into a commit plan.
pub(super) fn into_plan(tree: &DirTree) -> CommitPlan {
    let mut renames = Vec::new();
    let mut deletes = Vec::new();
    let mut stages = Vec::new();
    let mut prefix = String::new();
    collect(tree, &mut prefix, &mut renames, &mut deletes, &mut stages);

    let (saves, directs, places) = order_renames(renames);

    CommitPlan {
        saves,
        directs,
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
                stages.push(Action::Stage {
                    path: prefix.clone(),
                    ino: *ino,
                });
            }
            Target::BasePath(src) => {
                renames.push(Action::Rename {
                    dst: prefix.clone(),
                    src: src.clone(),
                });
            }
            Target::Tombstone => {
                deletes.push(Action::Delete {
                    path: prefix.clone(),
                });
            }
            Target::Passthrough => {}
        }

        if !matches!(node.target, Target::Tombstone) {
            collect(&node.children, prefix, renames, deletes, stages);
        }

        prefix.truncate(path_len);
    }
}

// ── Rename ordering (selective two-phase save/place) ──────────────────

/// `ancestor` is a strict path ancestor of `descendant`?
///
/// "/a" is an ancestor of "/a/b" but not of "/a", "/ab", or "/a-x".
fn is_path_ancestor(ancestor: &str, descendant: &str) -> bool {
    descendant.len() > ancestor.len()
        && descendant.as_bytes()[ancestor.len()] == b'/'
        && descendant[..ancestor.len()] == *ancestor
}

/// Compare paths in trie order: '/' is treated as '\x00' so that
/// children sort immediately after their parent, before unrelated
/// paths that happen to share a prefix (e.g. "/a/b" before "/a-x").
fn trie_order_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    for (&x, &y) in a.iter().zip(b.iter()) {
        let xk = if x == b'/' { 0 } else { x };
        let yk = if y == b'/' { 0 } else { y };
        match xk.cmp(&yk) {
            std::cmp::Ordering::Equal => {}
            other => return other,
        }
    }
    a.len().cmp(&b.len())
}

/// Split renames into saves, directs, and places.
///
/// - **saves**: move each conflicted source to a temp path, deepest source first.
/// - **directs**: conflict-free renames, single syscall each, DFS order.
/// - **places**: move each temp to its destination, DFS order.
fn order_renames(renames: Vec<Action>) -> (Vec<Action>, Vec<Action>, Vec<Action>) {
    if renames.is_empty() {
        return (Vec::new(), Vec::new(), Vec::new());
    }

    // Extract (dst, src) pairs, keeping original order index.
    let pairs: Vec<(&str, &str)> = renames
        .iter()
        .map(|a| match a {
            Action::Rename { dst, src } => (dst.as_str(), src.as_str()),
            _ => unreachable!(),
        })
        .collect();
    let n = pairs.len();

    // Detect conflicts via ancestor scan in trie-sorted order.
    //
    // Two renames conflict when any path (src or dst) of one is an
    // ancestor, descendant, or equal to any path of the other.  Rather
    // than checking every pair (O(n²)), we sort all 2n paths in trie
    // order and sweep with a stack: after popping non-ancestors, every
    // remaining stack entry is an ancestor-or-equal of the current path.
    // Any cross-rename match on the stack means a conflict.
    let mut needs_temp = vec![false; n];
    let mut tagged: Vec<(&str, usize)> = Vec::with_capacity(2 * n);
    for (i, &(dst, src)) in pairs.iter().enumerate() {
        tagged.push((src, i));
        tagged.push((dst, i));
    }
    tagged.sort_unstable_by(|a, b| trie_order_cmp(a.0, b.0));

    let mut stack: Vec<(&str, usize)> = Vec::new();
    for &(path, idx) in &tagged {
        while let Some(&(top, _)) = stack.last() {
            if top == path || is_path_ancestor(top, path) {
                break;
            }
            stack.pop();
        }
        for &(_, anc_idx) in &stack {
            if anc_idx != idx {
                needs_temp[idx] = true;
                needs_temp[anc_idx] = true;
            }
        }
        stack.push((path, idx));
    }

    // Direct renames: conflict-free, preserve DFS order.
    let directs: Vec<Action> = (0..n)
        .filter(|&i| !needs_temp[i])
        .map(|i| Action::Rename {
            dst: pairs[i].0.to_string(),
            src: pairs[i].1.to_string(),
        })
        .collect();

    // Conflicted renames: sort by source depth (deepest first) for saves.
    let mut temp_indices: Vec<usize> = (0..n).filter(|&i| needs_temp[i]).collect();
    temp_indices.sort_by(|&a, &b| pairs[b].1.len().cmp(&pairs[a].1.len()));

    let mut saves = Vec::with_capacity(temp_indices.len());
    let mut place_indexed: Vec<(usize, Action)> = Vec::with_capacity(temp_indices.len());

    for (temp_n, &orig_idx) in temp_indices.iter().enumerate() {
        let tmp = temp_path(temp_n);
        saves.push(Action::Rename {
            dst: tmp.clone(),
            src: pairs[orig_idx].1.to_string(),
        });
        place_indexed.push((
            orig_idx,
            Action::Rename {
                dst: pairs[orig_idx].0.to_string(),
                src: tmp,
            },
        ));
    }

    // Places in original DFS order (parent destinations before children).
    place_indexed.sort_by_key(|(idx, _)| *idx);
    let places = place_indexed.into_iter().map(|(_, a)| a).collect();

    (saves, directs, places)
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
        Action::Delete { path: path.into() }
    }

    fn rename(dest: &str, src: &str) -> Action {
        Action::Rename {
            dst: dest.into(),
            src: src.into(),
        }
    }

    fn get_renames(plan: &CommitPlan) -> Vec<(String, String)> {
        // Direct renames are already (dst, src).
        let mut result: Vec<(String, String)> = plan
            .directs
            .iter()
            .map(|op| match op {
                Action::Rename { dst, src } => (dst.clone(), src.clone()),
                _ => unreachable!(),
            })
            .collect();

        // Temp-based renames: pair places with saves to find original source.
        result.extend(plan.places.iter().map(|op| {
            let Action::Rename { dst, src: tmp } = op else {
                unreachable!()
            };
            let orig_src = plan
                .saves
                .iter()
                .find_map(|s| match s {
                    Action::Rename {
                        dst: save_dst,
                        src: save_src,
                    } if save_dst == tmp => Some(save_src.clone()),
                    _ => None,
                })
                .unwrap_or_else(|| tmp.clone());
            (dst.clone(), orig_src)
        }));

        result
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

    // ── Rename ordering: direct vs temp ─────────────────────────────────

    #[test]
    fn independent_rename_skips_temp() {
        // A single rename has no conflicts — should be direct.
        let plan = build(&[rename("/b", "/a")]).into_plan();
        assert_eq!(plan.directs.len(), 1);
        assert!(plan.saves.is_empty());
        assert!(plan.places.is_empty());
    }

    #[test]
    fn independent_renames_all_direct() {
        // Two renames with no path overlap — both direct.
        let plan = build(&[rename("/b", "/a"), rename("/d", "/c")]).into_plan();
        assert_eq!(plan.directs.len(), 2);
        assert!(plan.saves.is_empty());
        assert!(plan.places.is_empty());
    }

    #[test]
    fn conflicting_renames_use_temps() {
        // dst of one equals src of other — conflict.
        let plan = build(&[rename("/a", "/c"), rename("/c", "/b")]).into_plan();
        assert!(plan.directs.is_empty());
        assert_eq!(plan.saves.len(), 2);
        assert_eq!(plan.places.len(), 2);
    }

    #[test]
    fn nested_source_renames_use_temps() {
        // Parent/child sources conflict.
        let plan = build(&[rename("/x", "/dir/file"), rename("/y", "/dir")]).into_plan();
        assert!(plan.directs.is_empty());
        assert_eq!(plan.saves.len(), 2);
    }

    #[test]
    fn nested_destination_renames_use_temps() {
        // Parent/child destinations conflict — a direct place of the parent
        // would clobber the child via remove_dir_all.
        let plan = build(&[rename("/a", "/x"), rename("/a/b", "/y")]).into_plan();
        assert!(plan.directs.is_empty());
        assert_eq!(plan.saves.len(), 2);
        assert_eq!(plan.places.len(), 2);
    }

    #[test]
    fn mixed_direct_and_conflicted_renames() {
        // /b←/a and /d←/c are independent of each other but /b←/a
        // conflicts with /a←/e (dst_b == src_a).
        let plan =
            build(&[rename("/b", "/a"), rename("/d", "/c"), rename("/a", "/e")]).into_plan();
        assert_eq!(plan.directs.len(), 1, "/d←/c should be direct");
        assert_eq!(plan.saves.len(), 2, "/b←/a and /a←/e conflict");
    }

    #[test]
    fn prefix_not_ancestor_is_independent() {
        // /ab is NOT an ancestor of /a — these should be independent.
        let plan = build(&[rename("/ab", "/x"), rename("/a", "/y")]).into_plan();
        assert_eq!(plan.directs.len(), 2);
        assert!(plan.saves.is_empty());
    }

    #[test]
    fn saves_deepest_source_first() {
        // Sources /dir and /dir/f: save /dir/f first (deeper).
        let plan = build(&[rename("/x", "/dir/file"), rename("/y", "/dir")]).into_plan();
        assert_eq!(plan.saves.len(), 2);
        let Action::Rename { src, .. } = &plan.saves[0] else {
            unreachable!()
        };
        assert_eq!(src, "/dir/file", "deeper source must be saved first");
    }

    #[test]
    fn directs_preserve_dfs_order() {
        // Three renames with nested destinations but unrelated sources:
        // no conflicts, all become direct renames in DFS order.
        let plan = build(&[
            rename("/a", "/x"),
            rename("/a/b", "/y"),
            rename("/a/b/c", "/z"),
        ])
        .into_plan();
        let renames = get_renames(&plan);
        let idx_a = renames.iter().position(|p| p.0 == "/a").unwrap();
        let idx_ab = renames.iter().position(|p| p.0 == "/a/b").unwrap();
        let idx_abc = renames.iter().position(|p| p.0 == "/a/b/c").unwrap();
        assert!(idx_a < idx_ab, "/a must come before /a/b");
        assert!(idx_ab < idx_abc, "/a/b must come before /a/b/c");
    }

    #[test]
    fn saves_and_places_paired() {
        // Conflicting renames (dst of one = src of other): every
        // conflicted rename produces one save + one place.
        let plan = build(&[rename("/a", "/c"), rename("/c", "/b")]).into_plan();
        assert_eq!(plan.saves.len(), plan.places.len());
        assert_eq!(plan.saves.len(), 2);
    }

    #[test]
    fn source_resolution_in_chain() {
        // mv /a /b, mv /b /c, then mv /c/f /x.
        // Chain collapse: /c ← /a. Source resolution: /x ← /a/f.
        let plan =
            build(&[rename("/b", "/a"), rename("/c", "/b"), rename("/x", "/c/f")]).into_plan();
        let renames = get_renames(&plan);
        // /x should have source /a/f (resolved through chain).
        let x_rename = renames.iter().find(|(dst, _)| dst == "/x").unwrap();
        assert_eq!(x_rename.1, "/a/f", "source must be resolved to base path");
    }

    #[test]
    fn redirect_child_rename() {
        // mv /dir /other, then mv /dir/f /other/renamed.
        let plan =
            build(&[rename("/other", "/dir"), rename("/other/renamed", "/dir/f")]).into_plan();
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
        ])
        .into_plan();
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
        ])
        .into_plan();
        let renames = get_renames(&plan);
        assert_eq!(
            renames.len(),
            3,
            "3-way rotation should produce 3 logical renames"
        );
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
        assert_eq!(
            tree1, tree2,
            "tree should be a fixed point of build ∘ into_plan"
        );
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
        ])
        .into_plan();
        let mut renames = get_renames(&plan);
        renames.sort();
        assert_eq!(
            renames,
            vec![
                ("/a".to_string(), "/b".to_string()),
                ("/b".to_string(), "/a".to_string())
            ]
        );
    }

    #[test]
    fn idempotent_nested_stages() {
        assert_idempotent(&[add("/a", 1), add("/a/b", 2), add("/a/b/c", 3)]);
    }

    #[test]
    fn idempotent_delete_dir() {
        // Tombstoned dirs may have residual children in the tree that
        // get dropped by into_plan (it doesn't recurse into tombstoned
        // dirs). Verify the round-tripped tree is a subset with matching
        // targets.
        let tree1 = build(&[add("/dir/f", 1), delete("/dir/f"), delete("/dir")]);
        let plan = tree1.into_plan();
        let all = actions_without_temps(&plan);
        let tree2 = build(&all);

        let mut entries2 = Vec::new();
        tree2.for_each(|p, t| entries2.push((p.to_string(), t.clone())));
        for (path, target) in &entries2 {
            assert_eq!(
                tree1.get(path),
                Some(target),
                "tree2 entry {path} should match tree1"
            );
        }
    }
}

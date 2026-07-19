// yolo CLI — journal/plan.rs
//
// DirTree → CommitOps: convert the collapsed overlay state back into a
// minimal ordered list of filesystem mutations for commit.
//
// Implements `DirTree::into_plan()` — the inverse of `DirTree::build()`.
//
// ## Algorithm
//
// 1. **Collect**: DFS the tree, emitting actions for each node:
//      - stages  (StagedFile)  — copy a staged inode to base
//      - renames (BasePath)    — move a base path to a new location
//      - deletes (None)   — remove a base path
//    Scaffold nodes (end = None) emit no op; they exist only as scaffolding.
//    Recursion stops at None: a tombstoned dir is deleted whole
//    (`remove_dir_all`), so child actions are unnecessary.  Any children
//    staged before the delete are dead — the None overwrites them.
//
// 2. **Process renames**: every rename source is moved to a temp path,
//    deepest source first (so children are extracted before their parent
//    directory is moved).  The corresponding temp→destination placements
//    are emitted in DFS order (parent destinations before children).
//
//    Cost: 2n rename syscalls for n renames.
//
// 3. **Concatenate**: saves → places → deletes+stages.
//
// ## Why this order is correct
//
// Ordering principle: **None/StagedFile must not clobber BasePath**.
// BasePath is the only target type that references existing base state
// (it carries a source path it needs to read).  None and StagedFile
// are pure writes — they destroy or create, referencing nothing in base.
// If a None or StagedFile fires before a BasePath has read its
// source, it can clobber that source path.  Therefore all BasePath reads
// (saves) must complete before any None/StagedFile writes execute.
// Within the writers, places must precede deletes+stages because a
// stage may target a child of a rename destination.  Deletes and stages
// have no dependency on each other — the DirTree guarantees no stage
// writes under a deleted path — so they are interleaved in DFS order.

use super::tree::DirTree;
use super::types::Backing;
use std::path::Path;

/// One filesystem mutation in a commit plan.
///
/// Distinct from the journal's [`Action`](super::types::Action): commit applies
/// against the base filesystem and reads no pre-op backing, so a plan op carries
/// only what `apply_plan` needs — there are no `pre` fields to fabricate.
#[derive(Debug, Clone, PartialEq)]
pub enum CommitOp {
    /// Copy staged inode `ino` onto base `path`.
    Stage { path: String, ino: u32 },
    /// Move base `src` to base `dst` (also the save/place temp moves).
    Rename { dst: String, src: String },
    /// Remove base `path`.
    Delete { path: String },
}

/// A commit plan: the minimal set of filesystem mutations to apply.
///
/// Renames use a two-phase strategy: save all sources to temp paths,
/// then place temps at destinations.  Use `iter()` to iterate all
/// mutations in the correct execution order.
pub struct CommitPlan {
    /// Phase 1: save all rename sources to temp paths (deepest source first).
    saves: Vec<CommitOp>,
    /// Phase 2: places (temp→destination, DFS order) then deletes+stages
    /// (interleaved in DFS order).
    ops: Vec<CommitOp>,
}

impl CommitPlan {
    pub fn is_empty(&self) -> bool {
        self.saves.is_empty() && self.ops.is_empty()
    }

    pub fn len(&self) -> usize {
        // saves and places are paired — count as one rename each
        self.ops.len()
    }

    /// Iterate all ops in execution order.
    pub fn iter(&self) -> impl Iterator<Item = &CommitOp> {
        self.saves.iter().chain(&self.ops)
    }
}

/// Convert a DirTree into a commit plan.
pub(super) fn into_plan(tree: &DirTree, scratch: &Path) -> CommitPlan {
    let mut renames = Vec::new();
    let mut ops = Vec::new();
    let mut prefix = String::new();
    collect(tree, &mut prefix, &mut renames, &mut ops);

    let (saves, places) = process_renames(renames, scratch);

    // Phase 2: places first (parent dirs exist before children),
    // then deletes+stages (already in DFS order from collect).
    let mut all_ops = places;
    all_ops.extend(ops);

    CommitPlan {
        saves,
        ops: all_ops,
    }
}

fn collect(
    tree: &DirTree,
    prefix: &mut String,
    renames: &mut Vec<CommitOp>,
    ops: &mut Vec<CommitOp>,
) {
    for (name, node) in &tree.nodes {
        let path_len = prefix.len();
        prefix.push('/');
        prefix.push_str(name);

        // Commit reads the net state (`end`) only.
        match &node.new {
            Some(Backing::StagedFile(ino)) => {
                ops.push(CommitOp::Stage {
                    path: prefix.clone(),
                    ino: *ino,
                });
            }
            Some(Backing::BasePath(src)) => {
                renames.push(CommitOp::Rename {
                    dst: prefix.clone(),
                    src: src.clone(),
                });
            }
            Some(Backing::None) => {
                ops.push(CommitOp::Delete {
                    path: prefix.clone(),
                });
            }
            None => {} // scaffold
        }

        if !matches!(node.new, Some(Backing::None)) {
            collect(&node.children, prefix, renames, ops);
        }

        prefix.truncate(path_len);
    }
}

// ── Rename processing (two-phase save/place) ─────────────────────────

/// Split renames into saves and places.
///
/// - **saves**: move every source to a temp path, deepest source first
///   (so children are extracted before their parent directory moves).
/// - **places**: move each temp to its destination, in DFS order
///   (parent destinations before children).
fn process_renames(renames: Vec<CommitOp>, scratch: &Path) -> (Vec<CommitOp>, Vec<CommitOp>) {
    if renames.is_empty() {
        return (Vec::new(), Vec::new());
    }

    let pairs: Vec<(&str, &str)> = renames
        .iter()
        .map(|a| match a {
            CommitOp::Rename { dst, src } => (dst.as_str(), src.as_str()),
            _ => unreachable!(),
        })
        .collect();

    // Sort by source depth (deepest first) for saves.
    let mut by_depth: Vec<usize> = (0..pairs.len()).collect();
    by_depth.sort_by(|&a, &b| pairs[b].1.len().cmp(&pairs[a].1.len()));

    let mut saves = Vec::with_capacity(pairs.len());
    let mut places = Vec::with_capacity(pairs.len());

    for (temp_n, &orig_idx) in by_depth.iter().enumerate() {
        let tmp = temp_path(temp_n, scratch);
        saves.push(CommitOp::Rename {
            dst: tmp.clone(),
            src: pairs[orig_idx].1.to_string(),
        });
        places.push((
            orig_idx,
            CommitOp::Rename {
                dst: pairs[orig_idx].0.to_string(),
                src: tmp,
            },
        ));
    }

    // Places in original DFS order (parent destinations before children).
    places.sort_by_key(|(idx, _)| *idx);
    let places = places.into_iter().map(|(_, a)| a).collect();

    (saves, places)
}

/// Scratch path for a cycle-breaking rename. Lives in the session `.yolofs/`
/// dir, which is stable (outside any renamed subtree), on the same filesystem
/// as the committed files (so the save/place renames don't cross devices), and
/// owned by the invoking user — commit runs unprivileged, so `/` is not
/// writable.
fn temp_path(n: usize, scratch: &Path) -> String {
    scratch
        .join(format!(".yolofs-commit-tmp-{n}"))
        .to_string_lossy()
        .into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::journal::types::{Action, Record, Segment};

    fn build(actions: &[Action]) -> DirTree {
        DirTree::build(std::iter::once(Segment {
            records: actions.iter().cloned().map(Record::Action).collect(),
        }))
    }

    fn add(path: &str, ino: u32) -> Action {
        Action::Stage {
            path: path.into(),
            ino,
            pre: Backing::None,
        }
    }

    fn delete(path: &str) -> Action {
        Action::Delete {
            path: path.into(),
            pre: Backing::None,
        }
    }

    fn rename(dest: &str, src: &str) -> Action {
        rename_from(dest, src, src)
    }

    /// A rename whose source resolves (through a redirect chain) to base path
    /// `origin` — the kernel-faithful `src_pre`. A swap/rotation's return hop
    /// (`mv tmp a` after `mv a tmp`) resolves to the original base, not `tmp`.
    fn rename_from(dest: &str, src: &str, origin: &str) -> Action {
        Action::Rename {
            dst: dest.into(),
            src: src.into(),
            src_pre: Backing::BasePath(origin.into()),
            dst_pre: Backing::None,
        }
    }

    fn get_renames(plan: &CommitPlan) -> Vec<(String, String)> {
        // Pair places with saves to find original source.
        plan.ops
            .iter()
            .filter_map(|op| {
                let CommitOp::Rename { dst, src: tmp } = op else {
                    return None;
                };
                let orig_src = plan
                    .saves
                    .iter()
                    .find_map(|s| match s {
                        CommitOp::Rename {
                            dst: save_dst,
                            src: save_src,
                        } if save_dst == tmp => Some(save_src.clone()),
                        _ => None,
                    })
                    .unwrap_or_else(|| tmp.clone());
                Some((dst.clone(), orig_src))
            })
            .collect()
    }

    fn get_stages(plan: &CommitPlan) -> Vec<(String, u32)> {
        plan.ops
            .iter()
            .filter_map(|op| match op {
                CommitOp::Stage { path, ino } => Some((path.clone(), *ino)),
                _ => None,
            })
            .collect()
    }

    fn get_deletes(plan: &CommitPlan) -> Vec<String> {
        plan.ops
            .iter()
            .filter_map(|op| match op {
                CommitOp::Delete { path } => Some(path.clone()),
                _ => None,
            })
            .collect()
    }

    // ── CommitPlan basics ────────────────────────────────────────────────

    #[test]
    fn empty_tree_produces_empty_plan() {
        let plan = build(&[]).into_plan(Path::new("/scratch"));
        assert!(plan.is_empty());
    }

    #[test]
    fn non_empty_plan_is_not_empty() {
        let plan = build(&[add("/a", 1)]).into_plan(Path::new("/scratch"));
        assert!(!plan.is_empty());
    }

    #[test]
    fn staged_then_deleted_plan_is_not_empty() {
        let plan = build(&[add("/a", 1), delete("/a")]).into_plan(Path::new("/scratch"));
        assert!(!plan.is_empty(), "tombstone should still be non-empty");
    }

    // ── Plan generation: basic ────────────────────────────────────────

    #[test]
    fn plan_single_stage() {
        let plan = build(&[add("/a", 1)]).into_plan(Path::new("/scratch"));
        assert!(get_renames(&plan).is_empty());
        assert!(get_deletes(&plan).is_empty());
        assert_eq!(get_stages(&plan), vec![("/a".to_string(), 1)]);
    }

    #[test]
    fn plan_single_delete() {
        let plan = build(&[delete("/a")]).into_plan(Path::new("/scratch"));
        assert!(get_renames(&plan).is_empty());
        assert_eq!(get_deletes(&plan), vec!["/a"]);
        assert!(get_stages(&plan).is_empty());
    }

    #[test]
    fn plan_single_rename() {
        let plan = build(&[rename("/b", "/a")]).into_plan(Path::new("/scratch"));
        assert_eq!(
            get_renames(&plan),
            vec![("/b".to_string(), "/a".to_string())]
        );
        assert_eq!(get_deletes(&plan), vec!["/a"]);
        assert!(get_stages(&plan).is_empty());
    }

    #[test]
    fn plan_stage_then_delete_collapses() {
        let plan = build(&[add("/a", 1), delete("/a")]).into_plan(Path::new("/scratch"));
        assert!(get_renames(&plan).is_empty());
        assert_eq!(get_deletes(&plan), vec!["/a"]);
        assert!(get_stages(&plan).is_empty());
    }

    #[test]
    fn plan_stage_overwrite_collapses() {
        let plan = build(&[add("/a", 1), add("/a", 2)]).into_plan(Path::new("/scratch"));
        assert!(get_renames(&plan).is_empty());
        assert!(get_deletes(&plan).is_empty());
        assert_eq!(get_stages(&plan), vec![("/a".to_string(), 2)]);
    }

    #[test]
    fn plan_mixed_ops() {
        let plan = build(&[add("/a", 1), rename("/c", "/b"), delete("/d")])
            .into_plan(Path::new("/scratch"));
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
        let plan = build(&[add("/dir/f", 1), delete("/dir/f"), delete("/dir")])
            .into_plan(Path::new("/scratch"));
        assert!(get_renames(&plan).is_empty());
        assert!(get_stages(&plan).is_empty());
        assert_eq!(get_deletes(&plan), vec!["/dir"]);
    }

    // ── DFS ordering ───────────────────────────────────────────────────

    #[test]
    fn stages_parent_before_child() {
        let plan = build(&[add("/a/b/c", 3), add("/a", 1), add("/a/b", 2)])
            .into_plan(Path::new("/scratch"));
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
    fn independent_rename_single() {
        // A single rename — produces one logical rename.
        let plan = build(&[rename("/b", "/a")]).into_plan(Path::new("/scratch"));
        assert_eq!(plan.saves.len(), 1);
        assert_eq!(get_renames(&plan).len(), 1);
    }

    #[test]
    fn independent_renames_both_preserved() {
        // Two renames with no path overlap — both preserved.
        let plan =
            build(&[rename("/b", "/a"), rename("/d", "/c")]).into_plan(Path::new("/scratch"));
        assert_eq!(plan.saves.len(), 2);
        assert_eq!(get_renames(&plan).len(), 2);
    }

    #[test]
    fn conflicting_renames_use_temps() {
        // dst of one equals src of other — both renames preserved.
        let plan =
            build(&[rename("/a", "/c"), rename("/c", "/b")]).into_plan(Path::new("/scratch"));
        assert_eq!(plan.saves.len(), 2);
        assert_eq!(get_renames(&plan).len(), 2);
    }

    #[test]
    fn nested_source_renames_use_temps() {
        // Parent/child sources.
        let plan = build(&[rename("/x", "/dir/file"), rename("/y", "/dir")])
            .into_plan(Path::new("/scratch"));
        assert_eq!(plan.saves.len(), 2);
        assert_eq!(get_renames(&plan).len(), 2);
    }

    #[test]
    fn nested_destination_renames_use_temps() {
        // Parent/child destinations — saves required so parent doesn't
        // clobber child via remove_dir_all.
        let plan =
            build(&[rename("/a", "/x"), rename("/a/b", "/y")]).into_plan(Path::new("/scratch"));
        assert_eq!(plan.saves.len(), 2);
        assert_eq!(get_renames(&plan).len(), 2);
    }

    #[test]
    fn mixed_independent_and_conflicted_renames() {
        // /b←/a and /d←/c are independent of each other but /b←/a
        // conflicts with /a←/e (dst_b == src_a).  All three go through
        // save/place.
        let plan = build(&[rename("/b", "/a"), rename("/d", "/c"), rename("/a", "/e")])
            .into_plan(Path::new("/scratch"));
        assert_eq!(plan.saves.len(), 3, "all three renames use saves");
        assert_eq!(get_renames(&plan).len(), 3, "all three renames preserved");
    }

    #[test]
    fn prefix_not_ancestor_is_independent() {
        // /ab is NOT an ancestor of /a — these should be independent.
        let plan =
            build(&[rename("/ab", "/x"), rename("/a", "/y")]).into_plan(Path::new("/scratch"));
        assert_eq!(plan.saves.len(), 2);
        assert_eq!(get_renames(&plan).len(), 2);
    }

    #[test]
    fn saves_deepest_source_first() {
        // Sources /dir and /dir/f: save /dir/f first (deeper).
        let plan = build(&[rename("/x", "/dir/file"), rename("/y", "/dir")])
            .into_plan(Path::new("/scratch"));
        assert_eq!(plan.saves.len(), 2);
        let CommitOp::Rename { src, .. } = &plan.saves[0] else {
            unreachable!()
        };
        assert_eq!(src, "/dir/file", "deeper source must be saved first");
    }

    #[test]
    fn places_preserve_dfs_order() {
        // Three renames with nested destinations: places should be
        // in DFS order (parent before child).
        let plan = build(&[
            rename("/a", "/x"),
            rename("/a/b", "/y"),
            rename("/a/b/c", "/z"),
        ])
        .into_plan(Path::new("/scratch"));
        let renames = get_renames(&plan);
        let idx_a = renames.iter().position(|p| p.0 == "/a").unwrap();
        let idx_ab = renames.iter().position(|p| p.0 == "/a/b").unwrap();
        let idx_abc = renames.iter().position(|p| p.0 == "/a/b/c").unwrap();
        assert!(idx_a < idx_ab, "/a must come before /a/b");
        assert!(idx_ab < idx_abc, "/a/b must come before /a/b/c");
    }

    #[test]
    fn source_resolution_uses_record_pre() {
        // mv /a /b, mv /b /c (chain collapses to /c ← /a via node moves), then
        // mv /c/f /x. The kernel resolves /c/f's backing through the redirect and
        // records src_pre = BasePath("/a/f"); userspace uses it directly (no
        // tree-walk resolution), so /x ← /a/f.
        let plan = build(&[
            rename("/b", "/a"),
            rename("/c", "/b"),
            Action::Rename {
                dst: "/x".into(),
                src: "/c/f".into(),
                src_pre: Backing::BasePath("/a/f".into()),
                dst_pre: Backing::None,
            },
        ])
        .into_plan(Path::new("/scratch"));
        let renames = get_renames(&plan);
        let x_rename = renames.iter().find(|(dst, _)| dst == "/x").unwrap();
        assert_eq!(x_rename.1, "/a/f", "uses the record's src_pre directly");
    }

    #[test]
    fn redirect_child_rename() {
        // mv /dir /other, then mv /dir/f /other/renamed.
        let plan = build(&[rename("/other", "/dir"), rename("/other/renamed", "/dir/f")])
            .into_plan(Path::new("/scratch"));
        let renames = get_renames(&plan);
        assert_eq!(renames.len(), 2);
    }

    // ── Swap / rotation ─────────────────────────────────────────────────

    #[test]
    fn swap_produces_correct_logical_renames() {
        let plan = build(&[
            rename("/tmp", "/a"),
            rename("/a", "/b"),
            rename_from("/b", "/tmp", "/a"),
        ])
        .into_plan(Path::new("/scratch"));
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
            rename_from("/b", "/tmp", "/a"),
        ])
        .into_plan(Path::new("/scratch"));
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
        // Map plan ops back to journal actions to rebuild a tree. Renames come
        // from get_renames (which pairs places with saves to recover the
        // original source); stages/deletes map directly.
        get_renames(plan)
            .into_iter()
            .map(|(dst, src)| Action::Rename {
                src_pre: Backing::BasePath(src.clone()),
                dst,
                src,
                dst_pre: Backing::None,
            })
            .chain(plan.ops.iter().filter_map(|op| match op {
                CommitOp::Rename { .. } => None, // places already resolved above
                CommitOp::Stage { path, ino } => Some(Action::Stage {
                    path: path.clone(),
                    ino: *ino,
                    pre: Backing::None,
                }),
                CommitOp::Delete { path } => Some(Action::Delete {
                    path: path.clone(),
                    pre: Backing::None,
                }),
            }))
            .collect()
    }

    /// The net (committable) state: (path, end) pairs, scaffolds skipped. The
    /// `start` field is review-only metadata that `into_plan` discards, so
    /// idempotence is over the `end` projection, not the full node.
    fn ends(tree: &DirTree) -> Vec<(String, Backing)> {
        let mut v = Vec::new();
        tree.for_each(|p, t| v.push((p.to_string(), t.clone())));
        v
    }

    fn assert_idempotent(input: &[Action]) {
        let tree1 = build(input);
        let plan = tree1.into_plan(Path::new("/scratch"));
        let logical = actions_without_temps(&plan);
        let tree2 = build(&logical);
        assert_eq!(
            ends(&tree1),
            ends(&tree2),
            "net state should be a fixed point of build ∘ into_plan"
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
            rename_from("/b", "/tmp", "/a"),
        ])
        .into_plan(Path::new("/scratch"));
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
        let plan = tree1.into_plan(Path::new("/scratch"));
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

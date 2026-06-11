// yolo CLI — journal/tree.rs
//
// Dir tree builder. Applies journal records sequentially to build one sparse
// tree carrying both ends of a review comparison: each node has a `start` (the
// range-start old side, from the first touch's `pre`) and an `end` (the net
// overlay state, what commit/travel apply).
//
// Two rules govern absence (`end = Some(Target::Absence)`):
//   1. Rename vacate: when R moves a node away, place an absent end at the
//      vacated position.
//   2. Delete always tombstones: D on any node → end = Absence.
//
// Scaffold dirs that exist only to provide a path to deeper nodes carry
// `end = None` (and usually `start = None`).

use std::borrow::Borrow;
use std::collections::BTreeMap;

use super::types::*;

/// A node in the dir tree.
///
/// `end` is the net overlay state at the path (what commit/travel read); `start`
/// is the range-start old side (what `review`/`--diff` read for the old
/// content). Both are `None` on a scaffold dir — one that exists only to provide
/// a path to deeper nodes, with no touch of its own. A real touched entry has
/// `Some(end)` (and, after a fold over a non-empty range, `Some(start)`).
#[derive(Debug, Clone, PartialEq)]
pub struct DirNode {
    pub start: Option<Target>,
    pub end: Option<Target>,
    pub children: DirTree,
}

impl DirNode {
    /// A scaffold node: a path container with no touch of its own.
    fn scaffold() -> Self {
        DirNode {
            start: None,
            end: None,
            children: DirTree::new(),
        }
    }
}

/// A directory tree mapping child names to nodes. A `BTreeMap` so every
/// traversal (review listing, commit plan, travel serialization) is
/// byte-lexicographic per component, depth-first — deterministic by
/// construction.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct DirTree {
    pub nodes: BTreeMap<String, DirNode>,
}

impl DirTree {
    fn new() -> Self {
        Self {
            nodes: BTreeMap::new(),
        }
    }

    /// Apply a single journal action to the tree. First touch at a path assigns
    /// `start` (the range-start old side); later touches update only `end`.
    fn apply(&mut self, action: &Action) {
        match action {
            Action::Stage { path, ino, pre } => {
                self.touch(path.clone(), pre.clone(), Target::StagedFile(*ino));
            }
            Action::Delete { path, pre } => {
                self.touch(path.clone(), pre.clone(), Target::Absence);
            }
            Action::Rename {
                dst,
                src,
                src_pre,
                dst_pre,
            } => {
                self.apply_rename(dst.clone(), src.clone(), src_pre.clone(), dst_pre.clone());
            }
        }
    }

    /// Build a tree from a sequence of segments. Generic over `Borrow<Segment>`
    /// so callers can pass owned `Segment`s (consuming the journal) or `&Segment`
    /// (borrowing it — e.g. `Changeset::collect`, which runs once per segment for
    /// `--each`). Records are read by reference; only the net targets are kept.
    pub fn build<S: Borrow<Segment>>(segments: impl IntoIterator<Item = S>) -> Self {
        let mut tree = Self::new();
        for seg in segments {
            for record in &seg.borrow().records {
                match record {
                    Record::Action(action) => tree.apply(action),
                    // Notes are observational — no state change.
                    Record::Note(_) => {}
                    // Markers split segments and never appear inside one.
                    Record::Marker(_) => {}
                }
            }
        }
        tree
    }

    /// Convert the tree into a commit plan — the inverse of `build()`.
    /// `scratch` is a directory on the same filesystem as the committed files
    /// (the session `.yolofs/`) used to stage cycle-breaking rename temps.
    pub fn into_plan(&self, scratch: &std::path::Path) -> super::plan::CommitPlan {
        super::plan::into_plan(self, scratch)
    }

    /// Number of entries (files, dirs with metadata, negative entries) in the tree.
    /// Scaffold dirs (`end = None`) are excluded — they represent no staged change.
    pub fn len(&self) -> usize {
        self.nodes
            .values()
            .map(|n| usize::from(n.end.is_some()) + n.children.len())
            .sum()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Visit each (full-path, end-target) pair by reference. Scaffold dirs
    /// (`end = None`) are skipped — they carry no net state.
    pub fn for_each<F: FnMut(&str, &Target)>(&self, mut f: F) {
        self.visit_nodes(
            &mut |p, _start, end| {
                if let Some(end) = end {
                    f(p, end);
                }
            },
            &mut String::new(),
        );
    }

    /// Visit each (full-path, start, end) triple by reference, scaffolds
    /// included (review needs the old side and the net state together).
    pub fn for_each_change<F: FnMut(&str, Option<&Target>, Option<&Target>)>(&self, mut f: F) {
        self.visit_nodes(&mut f, &mut String::new());
    }

    /// Return true if any (path, target) pair matches the predicate.
    pub fn any<F: FnMut(&str, &Target) -> bool>(&self, mut f: F) -> bool {
        let mut found = false;
        self.for_each(|p, d| {
            if !found && f(p, d) {
                found = true;
            }
        });
        found
    }

    /// Look up the net (end) target by its full path (e.g. "/dir/file").
    /// Returns `None` if the path is not in the tree or is a scaffold.
    pub fn get(&self, path: &str) -> Option<&Target> {
        self.get_node(path).and_then(|n| n.end.as_ref())
    }

    /// Look up a node by its full path (e.g. "/dir/file").
    /// Returns `None` if the path is not in the tree.
    pub fn get_node(&self, path: &str) -> Option<&DirNode> {
        let mut parts = path.split('/').filter(|s| !s.is_empty()).peekable();
        let mut current = self;
        while let Some(part) = parts.next() {
            match current.nodes.get(part) {
                Some(node) => {
                    if parts.peek().is_none() {
                        return Some(node);
                    }
                    current = &node.children;
                }
                None => return None,
            }
        }
        None
    }

    /// Serialize the tree into a contiguous byte buffer for the travel ioctl.
    ///
    /// Wire format (all integers little-endian):
    ///   DirTree      := child_count:le16  DirNode[child_count]
    ///   DirNode      := name_len:le16  name:u8[name_len]
    ///                   Target
    ///                   child_count:le16  DirNode[child_count]   (children of this dir)
    ///   Target       := tag:u8  [payload]
    ///                   tag=1 Inode:  ino:le32
    ///                   tag=2 Path:   path_len:le16  path:u8[path_len]
    ///                   tag=3 None:   (no payload)
    ///
    /// Only the `end` target is serialized (commit/travel read `end` only).
    /// Scaffold dirs (`end = None`) use tag=2, path_len=0.
    /// File nodes always have child_count=0.
    pub fn serialize(&self) -> Vec<u8> {
        let mut buf = Vec::new();
        self.serialize_into(&mut buf);
        buf
    }

    fn serialize_into(&self, buf: &mut Vec<u8>) {
        // BTreeMap iteration is already name-sorted, so stream directly.
        // Scaffold dirs (end = None) are emitted as tag=2, path_len=0,
        // child_count=0 — the kernel parses them as pure no-ops.
        let count: u16 = self.nodes.len().try_into().expect("too many children");
        buf.extend_from_slice(&count.to_le_bytes());

        for (name, node) in &self.nodes {
            let name_bytes = name.as_bytes();
            let name_len: u16 = name_bytes.len().try_into().expect("name too long");
            buf.extend_from_slice(&name_len.to_le_bytes());
            buf.extend_from_slice(name_bytes);

            Self::serialize_end(&node.end, buf);
            node.children.serialize_into(buf);
        }
    }

    fn serialize_end(end: &Option<Target>, buf: &mut Vec<u8>) {
        match end {
            // Scaffold (no net state): tag=2 (Path), path_len=0.
            None => {
                buf.push(2);
                buf.extend_from_slice(&0u16.to_le_bytes());
            }
            Some(Target::StagedFile(ino)) => {
                assert!(*ino > 0, "inode ino must be non-zero");
                buf.push(1);
                buf.extend_from_slice(&ino.to_le_bytes());
            }
            Some(Target::BasePath(src)) => {
                buf.push(2);
                let bp = src.as_bytes();
                let bp_len: u16 = bp.len().try_into().expect("base_path too long");
                buf.extend_from_slice(&bp_len.to_le_bytes());
                buf.extend_from_slice(bp);
            }
            Some(Target::Absence) => {
                buf.push(3);
            }
        }
    }

    // ── Internal helpers ──────────────────────────────────────────────

    /// Walk to a path (owned), creating intermediate scaffold Dir nodes
    /// (`start`/`end = None`) as needed.  Extracts the leaf name from the path
    /// in-place via `drain`, avoiding allocation for the leaf component.
    fn walk_or_create_parent(&mut self, mut path: String) -> Option<(&mut DirTree, String)> {
        let last_slash = path.rfind('/')?;
        if last_slash + 1 >= path.len() {
            return None;
        }
        let mut current = self;
        for part in path[..last_slash].split('/').filter(|s| !s.is_empty()) {
            current
                .nodes
                .entry(part.to_string())
                .or_insert_with(DirNode::scaffold);
            current = &mut current.nodes.get_mut(part).unwrap().children;
        }
        path.drain(..last_slash + 1);
        Some((current, path))
    }

    /// Walk to a path (borrowed) for lookup only — no intermediate creation.
    fn walk_to_parent<'a>(&'a mut self, path: &'a str) -> Option<(&'a mut DirTree, &'a str)> {
        let last_slash = path.rfind('/')?;
        let name = &path[last_slash + 1..];
        if name.is_empty() {
            return None;
        }
        let mut current = self;
        for part in path[..last_slash].split('/').filter(|s| !s.is_empty()) {
            match current.nodes.get_mut(part) {
                Some(node) => current = &mut node.children,
                None => return None,
            }
        }
        Some((current, name))
    }

    /// First touch at `path`: assign `start` from `pre` (once) and set `end`,
    /// preserving any existing subtree. Used for S (`end = StagedFile`) and D
    /// (`end = Absence`).
    fn touch(&mut self, path: String, pre: Target, end: Target) {
        let Some((parent, name)) = self.walk_or_create_parent(path) else {
            return;
        };
        let node = parent.nodes.entry(name).or_insert_with(DirNode::scaffold);
        if node.start.is_none() {
            node.start = Some(pre);
        }
        node.end = Some(end);
    }

    /// Place a fresh leaf node at `path` (owned), replacing any existing leaf and
    /// creating scaffold ancestors as needed.
    fn place_node(&mut self, path: String, node: DirNode) {
        if let Some((parent, name)) = self.walk_or_create_parent(path) {
            parent.nodes.insert(name, node);
        }
    }

    /// Apply R rename. Both pre fields are kernel-provided operation-local
    /// backings (see [`Action::Rename`]); the fold trusts them and never
    /// re-derives a backing by walking the tree.
    ///
    /// The kernel re-pins the moved dentry with its own target, so `src_pre` is
    /// also the destination's post-rename backing: `dst.end = src_pre` verbatim.
    /// The detached source contributes only its children (which ride to `dst`)
    /// and its `start` (the move-carry old side). The destination's own
    /// first-touch `start` survives when the rename is not its first touch.
    fn apply_rename(&mut self, dst: String, src: String, src_pre: Target, dst_pre: Target) {
        if dst == src {
            return;
        }

        // Detach the source subtree: its children ride to dst; its own `start`
        // is the move-carry old side (carried to both dst and the source tomb).
        let (moved_start, moved_children) = match self.detach(&src) {
            Some(n) => (n.start, n.children),
            None => (None, DirTree::new()),
        };

        // Source tombstone: old side is the moved node's start if it had one,
        // else the source's own pre-op backing.
        self.place_node(
            src,
            DirNode {
                start: moved_start.clone().or_else(|| Some(src_pre.clone())),
                end: Some(Target::Absence),
                children: DirTree::new(),
            },
        );

        // A self-redirect (end would be BasePath(dst)) means the content
        // returned to its origin — a roundtrip (a→…→a). src_pre carries that
        // fact directly: a base/redirect source whose backing is dst itself.
        let is_self_redirect = matches!(&src_pre, Target::BasePath(p) if *p == dst);

        let Some((parent, name)) = self.walk_or_create_parent(dst) else {
            return;
        };
        let existing = parent.nodes.remove(name.as_str());

        if is_self_redirect {
            // No-op self-move: drop the node so commit emits no self-rename.
            // Roundtrip dir: keep the moved children but drop the self-redirect
            // end to a scaffold so commit doesn't self-move the directory.
            if !moved_children.nodes.is_empty() {
                parent.nodes.insert(
                    name,
                    DirNode {
                        start: None,
                        end: None,
                        children: moved_children,
                    },
                );
            }
            return;
        }

        // Destination `start` precedence: the moved node's start (move-carry),
        // then the destination's existing first-touch start (when the path was
        // already touched in-range — preserved, not clobbered), then the
        // op-local backing the rename displaced.
        let dst_start = moved_start
            .or_else(|| existing.and_then(|n| n.start))
            .or(Some(dst_pre));

        parent.nodes.insert(
            name,
            DirNode {
                start: dst_start,
                end: Some(src_pre),
                children: moved_children,
            },
        );
    }

    /// Detach a node from the tree, returning it. Returns None if not found.
    fn detach(&mut self, path: &str) -> Option<DirNode> {
        let (parent, name) = self.walk_to_parent(path)?;
        parent.nodes.remove(name)
    }

    /// Walk the tree by reference, calling `f` for each (path, start, end).
    /// Scaffold dirs (both `None`) are still visited — callers decide.
    fn visit_nodes<F: FnMut(&str, Option<&Target>, Option<&Target>)>(
        &self,
        f: &mut F,
        prefix: &mut String,
    ) {
        for (name, node) in &self.nodes {
            let path_len = prefix.len();
            prefix.push('/');
            prefix.push_str(name);

            if node.start.is_some() || node.end.is_some() {
                f(prefix, node.start.as_ref(), node.end.as_ref());
            }
            node.children.visit_nodes(f, prefix);

            prefix.truncate(path_len);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build(actions: &[Action]) -> DirTree {
        DirTree::build(std::iter::once(Segment {
            from: 0,
            records: actions.iter().cloned().map(Record::Action).collect(),
        }))
    }

    fn add(path: &str, ino: u32) -> Action {
        Action::Stage {
            path: path.into(),
            ino,
            pre: Target::Absence,
        }
    }

    fn delete(path: &str) -> Action {
        Action::Delete {
            path: path.into(),
            pre: Target::Absence,
        }
    }

    /// A rename of an *untouched base file*: `src_pre = BasePath(src)`, which is
    /// what the kernel records when the source resolves directly to its own base
    /// path. For a source that's already staged or a redirect (a chain's second
    /// hop), use [`rename_from`] / [`rename_staged`] — the kernel resolves those
    /// to the *origin* backing, not the immediate source path.
    fn rename(dest: &str, src: &str) -> Action {
        rename_from(dest, src, src)
    }

    /// A rename whose source resolves (through any redirect chain) to base path
    /// `origin` — the kernel-faithful `src_pre = BasePath(origin)`. The second
    /// hop of `mv a tmp; mv tmp a` resolves to the original base `a`, not the
    /// intermediate `tmp`.
    fn rename_from(dest: &str, src: &str, origin: &str) -> Action {
        Action::Rename {
            dst: dest.into(),
            src: src.into(),
            src_pre: Target::BasePath(origin.into()),
            dst_pre: Target::Absence,
        }
    }

    /// A rename of a staged source (inode `ino`): kernel-faithful
    /// `src_pre = StagedFile(ino)`. The moved inode rides to the destination.
    fn rename_staged(dest: &str, src: &str, ino: u32) -> Action {
        Action::Rename {
            dst: dest.into(),
            src: src.into(),
            src_pre: Target::StagedFile(ino),
            dst_pre: Target::Absence,
        }
    }

    // ── Basic insert ──────────────────────────────────────────────────

    #[test]
    fn add_single_file() {
        let tree = build(&[add("/a", 1)]);
        assert_eq!(tree.len(), 1);
        assert!(matches!(tree.get("/a"), Some(Target::StagedFile(1))));
    }

    #[test]
    fn modify_single_file() {
        let tree = build(&[add("/a", 1)]);
        assert_eq!(tree.len(), 1);
        assert!(matches!(tree.get("/a"), Some(Target::StagedFile(1))));
    }

    #[test]
    fn add_nested_file() {
        let tree = build(&[add("/dir/sub/file", 1)]);
        assert_eq!(tree.len(), 1);
        assert!(matches!(
            tree.get("/dir/sub/file"),
            Some(Target::StagedFile(1))
        ));
    }

    #[test]
    fn get_path_through_file_returns_none() {
        let tree = build(&[add("/file.txt", 1)]);
        assert!(
            tree.get("/file.txt/invalid").is_none(),
            "querying path through a file should return None"
        );
    }

    // ── Iteration order ───────────────────────────────────────────────

    #[test]
    fn for_each_visits_paths_in_component_sorted_dfs_order() {
        // Insert order is deliberately scrambled; iteration must be
        // byte-lexicographic per path component, depth-first. Note /a/b
        // precedes /a.txt: per-component DFS order is not flat string
        // order ("/a.txt" < "/a/b" as strings, since '.' < '/').
        let tree = build(&[
            add("/zeta", 1),
            add("/a.txt", 2),
            add("/mike", 3),
            add("/a/b", 4),
            add("/beta/x", 5),
            add("/beta", 6),
            add("/echo", 7),
            add("/delta/d2", 8),
            add("/delta/d1", 9),
            add("/kilo", 10),
        ]);
        let mut paths = Vec::new();
        tree.for_each(|p, _| paths.push(p.to_string()));
        assert_eq!(
            paths,
            vec![
                "/a/b",
                "/a.txt",
                "/beta",
                "/beta/x",
                "/delta/d1",
                "/delta/d2",
                "/echo",
                "/kilo",
                "/mike",
                "/zeta",
            ]
        );
    }

    // ── Delete ─────────────────────────────────────────────────────────

    #[test]
    fn add_then_delete_tombstones() {
        // Delete always tombstones — no cancel even for staged entries.
        let tree = build(&[add("/a", 1), delete("/a")]);
        assert_eq!(tree.len(), 1);
        assert!(matches!(tree.get("/a"), Some(Target::Absence)));
    }

    #[test]
    fn modify_then_delete_tombstone() {
        let tree = build(&[add("/a", 1), delete("/a")]);
        assert_eq!(tree.len(), 1);
        assert!(matches!(tree.get("/a"), Some(Target::Absence)));
    }

    #[test]
    fn delete_base_only_tombstone() {
        let tree = build(&[delete("/a")]);
        assert_eq!(tree.len(), 1);
        assert!(matches!(tree.get("/a"), Some(Target::Absence)));
    }

    // ── Rename (R) ────────────────────────────────────────────────────

    #[test]
    fn rename_added_file() {
        let tree = build(&[add("/a", 1), rename_staged("/b", "/a", 1)]);
        // A + R: rename always tombstones at source.
        // Destination gets the Inode.
        assert_eq!(tree.len(), 2);
        assert!(matches!(tree.get("/a"), Some(Target::Absence)));
        assert!(matches!(tree.get("/b"), Some(Target::StagedFile(1))));
    }

    #[test]
    fn rename_base_only_file() {
        let tree = build(&[rename("/b", "/a")]);
        // Base-only: Link at /b, Absence at /a
        assert_eq!(tree.len(), 2);
        assert!(matches!(tree.get("/a"), Some(Target::Absence)));
        assert!(matches!(tree.get("/b"), Some(Target::BasePath(from)) if from == "/a"));
    }

    // ── Rename chain ──────────────────────────────────────────────────

    #[test]
    fn rename_chain() {
        // a→b→c: the second hop's source /b redirects to base /a, so its
        // kernel-faithful src_pre is BasePath(/a), not BasePath(/b).
        let tree = build(&[rename("/b", "/a"), rename_from("/c", "/b", "/a")]);
        // a→b→c: Absence at /a, tombstone at /b (always tombstones at source), Link at /c
        assert_eq!(tree.len(), 3);
        assert!(matches!(tree.get("/a"), Some(Target::Absence)));
        assert!(matches!(tree.get("/b"), Some(Target::Absence)));
        assert!(matches!(tree.get("/c"), Some(Target::BasePath(from)) if from == "/a"));
    }

    // ── Source resolution (BasePath must reference immutable base) ────

    #[test]
    fn rename_uses_record_src_pre_not_tree_resolution() {
        // mv /a /dir, then mv /dir/f /x. The kernel resolves /dir/f's backing
        // through the redirect and records src_pre = BasePath("/a/f"); the tree
        // builder uses it directly (resolve_base_path is gone), so /x ← /a/f.
        let tree = build(&[
            rename("/dir", "/a"),
            Action::Rename {
                dst: "/x".into(),
                src: "/dir/f".into(),
                src_pre: Target::BasePath("/a/f".into()),
                dst_pre: Target::Absence,
            },
        ]);
        assert!(matches!(tree.get("/x"), Some(Target::BasePath(src)) if src == "/a/f"));
    }

    #[test]
    fn rename_dest_end_is_the_record_src_pre() {
        // Whatever pre the record carries for the source becomes the destination's
        // end, verbatim — no tree-walk backing resolution.
        let tree = build(&[Action::Rename {
            dst: "/x".into(),
            src: "/dir/sub/f".into(),
            src_pre: Target::BasePath("/b/f".into()),
            dst_pre: Target::Absence,
        }]);
        assert!(matches!(tree.get("/x"), Some(Target::BasePath(src)) if src == "/b/f"));
    }

    #[test]
    fn rename_no_redirect_keeps_base_path() {
        // mv /a /b — no redirect ancestor, source is already a base path.
        let tree = build(&[rename("/b", "/a")]);
        assert!(matches!(tree.get("/b"), Some(Target::BasePath(src)) if src == "/a"));
    }

    // ── Rename start/end composition (plan 58) ────────────────────────
    //
    // These use explicit `Action::Rename` with the pre fields the *kernel*
    // would actually write (the `rename()` helper fabricates an unfaithful
    // `src_pre = BasePath(src)` even for staged/redirect sources).

    #[test]
    fn rename_over_touched_dest_keeps_dest_first_touch_start() {
        // S /b ino1 (base /b copied up), then mv /a /b (base /a clobbers the
        // staged /b). /b's range-start old side is base /b — the rename must
        // not overwrite that first-touch `start` with the intermediate inode.
        let tree = build(&[
            Action::Stage {
                path: "/b".into(),
                ino: 1,
                pre: Target::BasePath("/b".into()),
            },
            Action::Rename {
                dst: "/b".into(),
                src: "/a".into(),
                src_pre: Target::BasePath("/a".into()),
                dst_pre: Target::StagedFile(1), // /b was staged when clobbered
            },
        ]);
        let node = tree.get_node("/b").expect("/b node");
        assert!(
            matches!(node.start, Some(Target::BasePath(ref p)) if p == "/b"),
            "dest first-touch start must survive the rename-over: {:?}",
            node.start
        );
        assert!(matches!(node.end, Some(Target::BasePath(ref p)) if p == "/a"));
    }

    #[test]
    fn delete_rename_over_restage_classifies_modified_not_added() {
        // rm /b; mv /a /b; edit /b. /b existed in base, so the net change is a
        // modify (base /b → new inode), not an add. The first-touch `start`
        // (base /b, from the delete) must survive the rename-over so classify
        // sees a present old side.
        let tree = build(&[
            Action::Delete {
                path: "/b".into(),
                pre: Target::BasePath("/b".into()),
            },
            Action::Rename {
                dst: "/b".into(),
                src: "/a".into(),
                src_pre: Target::BasePath("/a".into()),
                dst_pre: Target::Absence, // /b was a tombstone (negative) when clobbered
            },
            Action::Stage {
                path: "/b".into(),
                ino: 2,
                pre: Target::BasePath("/a".into()),
            },
        ]);
        let node = tree.get_node("/b").expect("/b node");
        assert!(
            matches!(node.start, Some(Target::BasePath(ref p)) if p == "/b"),
            "start should be base /b (the pre-delete content), got {:?}",
            node.start
        );
        assert!(matches!(node.end, Some(Target::StagedFile(2))));
    }

    #[test]
    fn rename_carries_moved_node_start_to_dest_child() {
        // Regression pin for move-carry (plan 56): vi /d/f (modify base d/f →
        // ino1), then mv /d /e. /e/f's old side is the moved child's own
        // `start` (base /d/f), carried verbatim across the directory move.
        let tree = build(&[
            Action::Stage {
                path: "/d/f".into(),
                ino: 1,
                pre: Target::BasePath("/d/f".into()),
            },
            Action::Rename {
                dst: "/e".into(),
                src: "/d".into(),
                src_pre: Target::BasePath("/d".into()),
                dst_pre: Target::Absence,
            },
        ]);
        let node = tree.get_node("/e/f").expect("/e/f node");
        assert!(
            matches!(node.start, Some(Target::BasePath(ref p)) if p == "/d/f"),
            "moved child's first-touch start should ride to /e/f: {:?}",
            node.start
        );
        assert!(matches!(node.end, Some(Target::StagedFile(1))));
    }

    #[test]
    fn rename_staged_source_carries_inode_to_dest_end() {
        // touch /a (ino1); mv /a /b. /b.end is the carried StagedFile(1) — the
        // record's src_pre verbatim, not re-derived. Pins the trust-the-record
        // contract for staged sources.
        let tree = build(&[
            Action::Stage {
                path: "/a".into(),
                ino: 1,
                pre: Target::Absence,
            },
            Action::Rename {
                dst: "/b".into(),
                src: "/a".into(),
                src_pre: Target::StagedFile(1),
                dst_pre: Target::Absence,
            },
        ]);
        assert!(matches!(tree.get("/b"), Some(Target::StagedFile(1))));
        assert!(matches!(tree.get("/a"), Some(Target::Absence)));
    }

    #[test]
    fn first_touch_start_rides_across_a_later_rename() {
        // Composition of the two rename behaviors: S /b ino1 seeds /b.start =
        // base /b; R /b←/a preserves that first-touch start (finding 1); then
        // R /c←/b move-carries it to /c. The third hop's source /b redirects to
        // base /a, so its kernel-faithful src_pre is BasePath(/a).
        let tree = build(&[
            Action::Stage {
                path: "/b".into(),
                ino: 1,
                pre: Target::BasePath("/b".into()),
            },
            Action::Rename {
                dst: "/b".into(),
                src: "/a".into(),
                src_pre: Target::BasePath("/a".into()),
                dst_pre: Target::StagedFile(1),
            },
            rename_from("/c", "/b", "/a"),
        ]);
        let c = tree.get_node("/c").expect("/c node");
        assert!(
            matches!(c.start, Some(Target::BasePath(ref p)) if p == "/b"),
            "first-touch start (base /b) should ride across both moves: {:?}",
            c.start
        );
        assert!(matches!(c.end, Some(Target::BasePath(ref p)) if p == "/a"));
        // The /b tombstone keeps the same first-touch old side.
        let b = tree.get_node("/b").expect("/b node");
        assert!(matches!(b.end, Some(Target::Absence)));
        assert!(matches!(b.start, Some(Target::BasePath(ref p)) if p == "/b"));
    }

    // ── Rename then delete ────────────────────────────────────────────

    #[test]
    fn rename_then_delete_base_file() {
        let tree = build(&[rename("/b", "/a"), delete("/b")]);
        // R(/b, /a): Link at /b, Absence at /a
        // D(/b): always tombstones → Absence at /b
        // Result: Absence at /a and Absence at /b
        assert_eq!(tree.len(), 2);
        assert!(matches!(tree.get("/a"), Some(Target::Absence)));
        assert!(matches!(tree.get("/b"), Some(Target::Absence)));
    }

    // ── Directory rename ──────────────────────────────────────────────

    #[test]
    fn dir_rename_moves_children() {
        let tree = build(&[
            add("/dir", 1),
            add("/dir/f1", 2),
            add("/dir/f2", 3),
            rename("/newdir", "/dir"),
        ]);
        assert!(tree.get("/newdir").is_some(), "missing /newdir");
        assert!(tree.get("/newdir/f1").is_some(), "missing /newdir/f1");
        assert!(tree.get("/newdir/f2").is_some(), "missing /newdir/f2");
        // Rename always tombstones at source
        assert!(matches!(tree.get("/dir"), Some(Target::Absence)));
        assert!(tree.get("/dir/f1").is_none(), "stale /dir/f1");
    }

    // ── Multiple modifies ─────────────────────────────────────────────

    #[test]
    fn multiple_modifies_last_wins() {
        let tree = build(&[add("/a", 1), add("/a", 2), add("/a", 3)]);
        assert_eq!(tree.len(), 1);
        assert!(matches!(tree.get("/a"), Some(Target::StagedFile(3))));
    }

    // ── Rename + modify at dest ───────────────────────────────────────

    #[test]
    fn rename_then_modify_at_dest() {
        // R(/b, /a) then A(/b, ino=5): base file renamed, then overwritten at dest.
        // Tree: Inode(ino=5) at /b, Absence at /a.
        let tree = build(&[rename("/b", "/a"), add("/b", 5)]);
        assert_eq!(tree.len(), 2);
        assert!(matches!(tree.get("/a"), Some(Target::Absence)));
        assert!(matches!(tree.get("/b"), Some(Target::StagedFile(5))));
    }

    // ── Rename over tombstone ─────────────────────────────────────────

    #[test]
    fn rename_over_tombstone() {
        // Delete /b (creates tombstone), then rename /a → /b.
        // The rename replaces the tombstone with a Link.
        let tree = build(&[delete("/b"), rename("/b", "/a")]);
        assert_eq!(tree.len(), 2);
        assert!(matches!(tree.get("/a"), Some(Target::Absence)));
        assert!(matches!(tree.get("/b"), Some(Target::BasePath(from)) if from == "/a"));
    }

    // ── Add then rename (staged rename) ───────────────────────────────

    #[test]
    fn add_then_rename_preserves_inode() {
        // A(/a, ino=1) then R(/b, /a): staged file moved, inode preserved.
        let tree = build(&[add("/a", 1), rename_staged("/b", "/a", 1)]);
        // Rename always tombstones at source.
        // Inode moved to /b.
        assert_eq!(tree.len(), 2);
        assert!(matches!(tree.get("/a"), Some(Target::Absence)));
        assert!(matches!(tree.get("/b"), Some(Target::StagedFile(1))));
    }

    // ── Create-delete-recreate ────────────────────────────────────────

    #[test]
    fn add_delete_add_same_path() {
        // A + D → tombstone, then A replaces tombstone with new inode.
        let tree = build(&[add("/a", 1), delete("/a"), add("/a", 2)]);
        assert_eq!(tree.len(), 1);
        assert!(matches!(tree.get("/a"), Some(Target::StagedFile(2))));
    }

    #[test]
    fn modify_delete_recreate() {
        // A + D → tombstone, then A replaces tombstone with new inode.
        let tree = build(&[add("/a", 1), delete("/a"), add("/a", 2)]);
        assert_eq!(tree.len(), 1);
        // A over Absence: the new inode replaces the tombstone.
        assert!(matches!(tree.get("/a"), Some(Target::StagedFile(2))));
    }

    // ── delete_dir ────────────────────────────────────────────────────

    #[test]
    fn delete_dir_base_only_tombstone() {
        let tree = build(&[delete("/d")]);
        assert_eq!(tree.len(), 1);
        assert!(matches!(tree.get("/d"), Some(Target::Absence)));
    }

    #[test]
    fn add_dir_then_delete_dir_tombstones() {
        // Delete always tombstones — no cancel even for staged dirs.
        let tree = build(&[add("/d", 1), delete("/d")]);
        assert_eq!(tree.len(), 1);
        assert!(matches!(tree.get("/d"), Some(Target::Absence)));
    }

    #[test]
    fn delete_dir_with_children() {
        // Deleting a dir tombstones it; children remain in the subtree.
        let tree = build(&[add("/d", 1), add("/d/f1", 2), add("/d/f2", 3), delete("/d")]);
        assert_eq!(tree.len(), 3);
        assert!(matches!(tree.get("/d"), Some(Target::Absence)));
        assert!(matches!(tree.get("/d/f1"), Some(Target::StagedFile(2))));
        assert!(matches!(tree.get("/d/f2"), Some(Target::StagedFile(3))));
    }

    #[test]
    fn delete_dir_preserves_children() {
        let tree = build(&[
            add("/d", 1),
            add("/d/f", 2),
            Action::Delete {
                path: "/d".into(),
                pre: Target::Absence,
            },
        ]);
        // Delete always tombstones. Children remain in the subtree.
        assert_eq!(tree.len(), 2);
        assert!(matches!(tree.get("/d"), Some(Target::Absence)));
        assert!(matches!(tree.get("/d/f"), Some(Target::StagedFile(2))));
    }

    #[test]
    fn delete_base_dir_then_add_child() {
        // Delete a base directory, then add a file under it.
        // The tombstone should be a Dir node so walk_to_parent succeeds.
        let tree = build(&[delete("/d"), add("/d/f1", 1)]);
        assert!(matches!(tree.get("/d"), Some(Target::Absence)));
        assert!(matches!(tree.get("/d/f1"), Some(Target::StagedFile(1))));
    }

    // ── Delete intermediate directory ─────────────────────────────────

    #[test]
    fn delete_intermediate_dir_creates_tombstone() {
        // Add /a/b/c/file — creates intermediate /a, /a/b, /a/b/c.
        // Delete /a/b → should create Absence for the intermediate dir.
        let tree = build(&[add("/a/b/c/file", 1), delete("/a/b")]);
        // /a/b was intermediate (Dir(passthrough,..)) → treated as base → negative dentry.
        assert!(
            matches!(tree.get("/a/b"), Some(Target::Absence)),
            "intermediate dir should get Absence: {:?}",
            tree
        );
    }

    // ── Snapshot/Travel records ignored ────────────────────────────

    #[test]
    fn snapshot_records_ignored_in_stream() {
        let tree = build(&[
            add("/x", 1),
            Action::Stage {
                path: "/x".into(),
                ino: 2,
                pre: Target::Absence,
            },
        ]);
        assert_eq!(tree.len(), 1);
        assert!(matches!(tree.get("/x"), Some(Target::StagedFile(2))));
    }

    #[test]
    fn travel_records_ignored_in_stream() {
        let tree = build(&[add("/a", 1), add("/b", 2)]);
        assert_eq!(tree.len(), 2);
        assert!(matches!(tree.get("/a"), Some(Target::StagedFile(1))));
        assert!(matches!(tree.get("/b"), Some(Target::StagedFile(2))));
    }

    // ── Self-rename / roundtrip / cycle ───────────────────────────────

    #[test]
    fn self_rename_is_noop() {
        let tree = build(&[rename("/a", "/a")]);
        assert!(tree.is_empty(), "R(a,a) should be a no-op: {:?}", tree);
    }

    #[test]
    fn roundtrip_rename_produces_passthrough() {
        // a→tmp→a: file roundtrip removes the node from the tree,
        // but /tmp gets a tombstone (rename always tombstones at source).
        // The return hop's source /tmp redirects to base /a — src_pre = /a.
        let tree = build(&[rename("/tmp", "/a"), rename_from("/a", "/tmp", "/a")]);
        assert_eq!(tree.len(), 1, "/tmp tombstone is the only staged change");
        // File roundtrip — node at /a removed entirely from tree.
        assert!(
            tree.nodes.get("a").is_none(),
            "expected file roundtrip to remove node, got {:?}",
            tree.nodes.get("a")
        );
        assert!(
            matches!(tree.get("/tmp"), Some(Target::Absence)),
            "/tmp should be tombstoned: {:?}",
            tree
        );
    }

    #[test]
    fn roundtrip_rename_dir_removed() {
        // Dir roundtrip (a→tmp→a) — base-only renames with empty
        // children are removed (same as file roundtrip).
        // /tmp gets a tombstone (rename always tombstones at source).
        let tree = build(&[rename("/tmp", "/a"), rename_from("/a", "/tmp", "/a")]);
        assert_eq!(tree.len(), 1, "/tmp tombstone is the only staged change");
        assert!(
            tree.nodes.get("a").is_none(),
            "base-only roundtrip with no children should remove node, got {:?}",
            tree.nodes.get("a")
        );
        assert!(
            matches!(tree.get("/tmp"), Some(Target::Absence)),
            "/tmp should be tombstoned: {:?}",
            tree
        );
    }

    #[test]
    fn three_cycle_swap() {
        // a→tmp, b→a, tmp→b: swaps a and b via tmp. The final hop's source
        // /tmp redirects to base /a (from the first hop) — src_pre = /a.
        let tree = build(&[
            rename("/tmp", "/a"),
            rename("/a", "/b"),
            rename_from("/b", "/tmp", "/a"),
        ]);
        assert!(!tree.is_empty(), "swap should produce dentries: {:?}", tree);
        // /a should be a Renamed from /b, /b should be a Renamed from /a
        assert!(
            matches!(tree.get("/a"), Some(Target::BasePath(from)) if from == "/b"),
            "a should come from b: {:?}",
            tree
        );
        assert!(
            matches!(tree.get("/b"), Some(Target::BasePath(from)) if from == "/a"),
            "b should come from a: {:?}",
            tree
        );
    }

    #[test]
    fn three_step_roundtrip_rename_produces_passthrough() {
        // a→b→c→a: file roundtrip removes /a from the tree,
        // but /b and /c get tombstones (rename always tombstones at source).
        // Each later hop's source redirects back to base /a — src_pre = /a.
        let tree = build(&[
            rename("/b", "/a"),
            rename_from("/c", "/b", "/a"),
            rename_from("/a", "/c", "/a"),
        ]);
        assert_eq!(
            tree.len(),
            2,
            "tombstones at /b and /c after 3-step roundtrip"
        );
        assert!(
            tree.nodes.get("a").is_none(),
            "expected file roundtrip to remove node, got {:?}",
            tree.nodes.get("a")
        );
        assert!(
            matches!(tree.get("/b"), Some(Target::Absence)),
            "/b should be tombstoned: {:?}",
            tree
        );
        assert!(
            matches!(tree.get("/c"), Some(Target::Absence)),
            "/c should be tombstoned: {:?}",
            tree
        );
    }

    // ── Empty tree ────────────────────────────────────────────────────

    #[test]
    fn empty_tree_dentries() {
        let tree = build(&[]);
        assert!(tree.is_empty());
    }

    // ── Symlink ────────────────────────────────────────────────────────

    #[test]
    fn add_symlink() {
        let tree = build(&[add("/link", 1)]);
        assert_eq!(tree.len(), 1);
        assert!(matches!(tree.get("/link"), Some(Target::StagedFile(1))));
    }

    #[test]
    fn rename_symlink() {
        let tree = build(&[rename("/new", "/old")]);
        assert_eq!(tree.len(), 2);
        assert!(matches!(tree.get("/old"), Some(Target::Absence)));
        assert!(matches!(tree.get("/new"), Some(Target::BasePath(_))));
    }

    #[test]
    fn add_then_delete_symlink_tombstones() {
        // Delete always tombstones — no cancel even for staged symlinks.
        let tree = build(&[
            add("/link", 1),
            Action::Delete {
                path: "/link".into(),
                pre: Target::Absence,
            },
        ]);
        assert_eq!(tree.len(), 1);
        assert!(matches!(tree.get("/link"), Some(Target::Absence)));
    }

    // ── Base-only directory rename ────────────────────────────────────

    #[test]
    fn rename_base_only_dir() {
        let tree = build(&[rename("/new", "/old")]);
        assert_eq!(tree.len(), 2);
        assert!(matches!(tree.get("/old"), Some(Target::Absence)));
        assert!(matches!(tree.get("/new"), Some(Target::BasePath(from)) if from == "/old"));
    }

    #[test]
    fn rename_dir_moves_subtree() {
        let tree = build(&[
            add("/old", 1),
            add("/old/file", 2),
            Action::Rename {
                src: "/old".into(),
                dst: "/new".into(),
                src_pre: Target::BasePath("/old".into()),
                dst_pre: Target::Absence,
            },
        ]);
        // Rename always tombstones at source
        assert!(matches!(tree.get("/old"), Some(Target::Absence)));
        assert!(
            tree.get("/old/file").is_none(),
            "old subtree should be gone"
        );
        assert!(tree.get("/new").is_some(), "new dir should exist");
        assert!(
            tree.get("/new/file").is_some(),
            "subtree should move with dir"
        );
    }

    // ── Dir rename + subsequent child operations ──────────────────────

    #[test]
    fn dir_rename_then_add_child() {
        let tree = build(&[
            add("/old", 1),
            rename("/new", "/old"),
            add("/new/child.txt", 2),
        ]);
        assert!(tree.get("/new").is_some(), "missing /new");
        assert!(
            tree.get("/new/child.txt").is_some(),
            "missing /new/child.txt"
        );
        assert!(
            matches!(tree.get("/new/child.txt"), Some(Target::StagedFile(2))),
            "child should be Inode(ino=2): {:?}",
            tree
        );
    }

    #[test]
    fn dir_rename_then_delete_moved_child() {
        let tree = build(&[
            add("/old", 1),
            add("/old/f1", 2),
            rename("/new", "/old"),
            delete("/new/f1"),
        ]);
        // Delete always tombstones. /new has the dir, /new/f1 is tombstoned.
        assert!(tree.get("/new").is_some(), "missing /new");
        assert!(
            matches!(tree.get("/new/f1"), Some(Target::Absence)),
            "/new/f1 should be tombstoned: {:?}",
            tree
        );
    }

    // ── Rename over intermediate directory ────────────────────────────

    #[test]
    fn rename_into_intermediate_dir_position() {
        // A(/a/b/c/file) creates intermediates /a, /a/b, /a/b/c.
        // R(/other, /a/b) renames intermediate /a/b (which is a Dir(passthrough, ...) node).
        let tree = build(&[add("/a/b/c/file", 1), rename("/other", "/a/b")]);
        // /a/b was an intermediate dir → source_had_base=true → negative dentry at /a/b.
        assert!(
            matches!(tree.get("/a/b"), Some(Target::Absence)),
            "/a/b should be negative dentry: {:?}",
            tree
        );
        // /other gets the subtree with /other/c/file.
        assert!(tree.get("/other/c/file").is_some(), "missing /other/c/file");
        assert!(
            matches!(tree.get("/other/c/file"), Some(Target::StagedFile(1))),
            "/other/c/file should be Inode(ino=1): {:?}",
            tree
        );
        // /other should have a redirect dentry (from intermediate dir rename)
        assert!(
            matches!(tree.get("/other"), Some(Target::BasePath(from)) if from == "/a/b"),
            "/other should be redirect from /a/b: {:?}",
            tree
        );
    }

    #[test]
    fn replace_then_delete_tombstones_destination() {
        // Rename /a → /b (both base-only), then delete /b.
        // /b existed in base, so deleting the Link must leave a Absence
        // to hide the base content. Without it, base /b reappears.
        let cs = build(&[
            Action::Rename {
                src: "/a".into(),
                dst: "/b".into(),
                src_pre: Target::BasePath("/a".into()),
                dst_pre: Target::Absence,
            },
            Action::Delete {
                path: "/b".into(),
                pre: Target::Absence,
            },
        ]);
        // Both /a and /b should be tombstoned.
        assert!(
            matches!(cs.get("/a"), Some(Target::Absence)),
            "/a should be Absence: {:?}",
            cs
        );
        assert!(
            matches!(cs.get("/b"), Some(Target::Absence)),
            "/b should be Absence (base content): {:?}",
            cs
        );
    }

    // ── serialize tests ───────────────────────────────────────────────

    #[test]
    fn serialize_empty_tree() {
        let tree = DirTree::new();
        assert_eq!(tree.serialize(), vec![0x00, 0x00]);
    }

    #[test]
    fn serialize_single_inode_file() {
        let tree = build(&[add("/_f", 1)]);
        let buf = tree.serialize();
        let mut expected = Vec::new();
        expected.extend_from_slice(&1u16.to_le_bytes()); // child_count = 1
        // Node "_f"
        expected.extend_from_slice(&2u16.to_le_bytes()); // name_len = 2
        expected.extend_from_slice(b"_f");
        // Target: kind=1(StagedInode), ino=1
        expected.push(1); // kind
        expected.extend_from_slice(&1u32.to_le_bytes()); // ino
        expected.extend_from_slice(&0u16.to_le_bytes()); // child_count = 0
        assert_eq!(buf, expected);
    }

    #[test]
    fn serialize_single_tombstone() {
        let tree = build(&[
            Action::Stage {
                path: "/old".into(),
                ino: 1,
                pre: Target::Absence,
            },
            Action::Delete {
                path: "/old".into(),
                pre: Target::Absence,
            },
        ]);
        let buf = tree.serialize();
        let mut expected = Vec::new();
        expected.extend_from_slice(&1u16.to_le_bytes()); // child_count = 1
        expected.extend_from_slice(&3u16.to_le_bytes()); // name_len = 3
        expected.extend_from_slice(b"old");
        // Target: kind=3(None/negative)
        expected.push(3);
        expected.extend_from_slice(&0u16.to_le_bytes()); // child_count = 0
        assert_eq!(buf, expected);
    }

    #[test]
    fn serialize_single_link() {
        let tree = build(&[Action::Rename {
            src: "/a.txt".into(),
            dst: "/b.txt".into(),
            src_pre: Target::BasePath("/a.txt".into()),
            dst_pre: Target::Absence,
        }]);
        let buf = tree.serialize();
        let mut cursor = 0usize;
        let child_count = u16::from_le_bytes([buf[cursor], buf[cursor + 1]]);
        cursor += 2;
        assert_eq!(child_count, 2);

        // Children are sorted: "a.txt" before "b.txt"
        // Node 1: "a.txt" — negative dentry
        let name_len = u16::from_le_bytes([buf[cursor], buf[cursor + 1]]) as usize;
        cursor += 2;
        assert_eq!(&buf[cursor..cursor + name_len], b"a.txt");
        cursor += name_len;
        assert_eq!(buf[cursor], 3); // kind=None/negative
        cursor += 1;
        let cc = u16::from_le_bytes([buf[cursor], buf[cursor + 1]]);
        cursor += 2;
        assert_eq!(cc, 0);

        // Node 2: "b.txt" — redirect to /a.txt
        let name_len = u16::from_le_bytes([buf[cursor], buf[cursor + 1]]) as usize;
        cursor += 2;
        assert_eq!(&buf[cursor..cursor + name_len], b"b.txt");
        cursor += name_len;
        assert_eq!(buf[cursor], 2); // kind=REDIRECT
        cursor += 1;
        // Trailing: base_len + base_path
        let base_len = u16::from_le_bytes([buf[cursor], buf[cursor + 1]]) as usize;
        cursor += 2;
        assert_eq!(&buf[cursor..cursor + base_len], b"/a.txt");
        cursor += base_len;
        let cc = u16::from_le_bytes([buf[cursor], buf[cursor + 1]]);
        cursor += 2;
        assert_eq!(cc, 0);

        assert_eq!(cursor, buf.len());
    }

    #[test]
    fn serialize_nested_directories() {
        // /dir (staged dir inode) with /dir/file inside
        let tree = build(&[add("/dir", 10), add("/dir/file", 20)]);
        let buf = tree.serialize();
        let mut cursor = 0usize;

        // Root child_count = 1
        let cc = u16::from_le_bytes([buf[cursor], buf[cursor + 1]]);
        cursor += 2;
        assert_eq!(cc, 1);

        // Node "dir": name_len=3, name="dir", dentry(kind=1,ino=10), child_count=1
        let name_len = u16::from_le_bytes([buf[cursor], buf[cursor + 1]]) as usize;
        cursor += 2;
        assert_eq!(&buf[cursor..cursor + name_len], b"dir");
        cursor += name_len;
        assert_eq!(buf[cursor], 1); // kind=STAGED_INODE
        cursor += 1;
        let ino = u32::from_le_bytes(buf[cursor..cursor + 4].try_into().unwrap());
        assert_eq!(ino, 10);
        cursor += 4;

        // child_count for "dir" subtree = 1
        let cc = u16::from_le_bytes([buf[cursor], buf[cursor + 1]]);
        cursor += 2;
        assert_eq!(cc, 1);

        // Node "file": name_len=4, name="file", dentry(kind=1,ino=20), child_count=0
        let name_len = u16::from_le_bytes([buf[cursor], buf[cursor + 1]]) as usize;
        cursor += 2;
        assert_eq!(&buf[cursor..cursor + name_len], b"file");
        cursor += name_len;
        assert_eq!(buf[cursor], 1); // kind=STAGED_INODE
        cursor += 1;
        let ino = u32::from_le_bytes(buf[cursor..cursor + 4].try_into().unwrap());
        assert_eq!(ino, 20);
        cursor += 4;
        let cc = u16::from_le_bytes([buf[cursor], buf[cursor + 1]]);
        cursor += 2;
        assert_eq!(cc, 0);

        assert_eq!(cursor, buf.len());
    }

    #[test]
    fn serialize_passthrough_dir() {
        // /dir/file where /dir has no own dirent (pass-through)
        let tree = build(&[add("/dir/file", 5)]);
        let buf = tree.serialize();
        let mut cursor = 0usize;

        // Root child_count = 1
        let cc = u16::from_le_bytes([buf[cursor], buf[cursor + 1]]);
        cursor += 2;
        assert_eq!(cc, 1);

        // Node "dir": kind=2 path_len=0 (passthrough scaffold)
        let name_len = u16::from_le_bytes([buf[cursor], buf[cursor + 1]]) as usize;
        cursor += 2;
        assert_eq!(&buf[cursor..cursor + name_len], b"dir");
        cursor += name_len;
        assert_eq!(buf[cursor], 2); // kind=REDIRECT (passthrough scaffold)
        cursor += 1;
        let path_len = u16::from_le_bytes([buf[cursor], buf[cursor + 1]]);
        assert_eq!(path_len, 0); // empty path
        cursor += 2;

        // child_count = 1
        let cc = u16::from_le_bytes([buf[cursor], buf[cursor + 1]]);
        cursor += 2;
        assert_eq!(cc, 1);

        // Node "file"
        let name_len = u16::from_le_bytes([buf[cursor], buf[cursor + 1]]) as usize;
        cursor += 2;
        assert_eq!(&buf[cursor..cursor + name_len], b"file");
        cursor += name_len;
        assert_eq!(buf[cursor], 1); // kind=STAGED_INODE
        cursor += 1;
        cursor += 4; // ino
        let cc = u16::from_le_bytes([buf[cursor], buf[cursor + 1]]);
        cursor += 2;
        assert_eq!(cc, 0);

        assert_eq!(cursor, buf.len());
    }

    #[test]
    fn serialize_children_sorted_by_name() {
        let tree = build(&[add("/z", 1), add("/a", 2), add("/m", 3)]);
        let buf = tree.serialize();
        let mut cursor = 2usize; // skip root child_count

        // Read names in order — each node: name_len(2) + name + kind(1) + ino(4) + cc(2)
        let mut names = Vec::new();
        for _ in 0..3 {
            let name_len = u16::from_le_bytes([buf[cursor], buf[cursor + 1]]) as usize;
            cursor += 2;
            names.push(
                std::str::from_utf8(&buf[cursor..cursor + name_len])
                    .unwrap()
                    .to_string(),
            );
            cursor += name_len;
            cursor += 1 + 4; // kind + ino (StagedInode)
            cursor += 2; // child_count
        }
        assert_eq!(names, vec!["a", "m", "z"]);
    }

    #[test]
    fn serialize_empty_passthrough_dir_emitted() {
        // An empty passthrough dir is emitted as-is (tag=2, path_len=0,
        // child_count=0); the kernel parses it as a pure no-op.
        let mut tree = DirTree::new();
        tree.nodes.insert(
            "empty".to_string(),
            DirNode {
                start: None,
                end: None,
                children: DirTree::new(),
            },
        );
        let buf = tree.serialize();
        let mut expected = Vec::new();
        expected.extend_from_slice(&1u16.to_le_bytes()); // child_count = 1
        expected.extend_from_slice(&5u16.to_le_bytes()); // name_len = 5
        expected.extend_from_slice(b"empty");
        expected.push(2); // tag=2 (PATH/passthrough)
        expected.extend_from_slice(&0u16.to_le_bytes()); // path_len = 0
        expected.extend_from_slice(&0u16.to_le_bytes()); // child_count = 0
        assert_eq!(buf, expected);
    }

    #[test]
    fn serialize_stale_intermediates_after_cancel() {
        // Add a deeply nested file then delete it. Delete always tombstones,
        // so the leaf gets a tombstone. Scaffold dir intermediates remain.
        let tree = build(&[add("/a/b/c/file", 1), delete("/a/b/c/file")]);
        assert_eq!(tree.len(), 1, "tombstone at /a/b/c/file");
        // Serialization still succeeds (doesn't panic).
        let _buf = tree.serialize();
    }

    #[test]
    fn serialize_partial_stale_intermediates() {
        // /a/b/c/file1 added + deleted (tombstone), and /a/x still exists.
        let tree = build(&[
            add("/a/b/c/file1", 1),
            add("/a/x", 2),
            delete("/a/b/c/file1"),
        ]);
        assert_eq!(tree.len(), 2, "/a/x survives, /a/b/c/file1 is tombstoned");
        let buf = tree.serialize();
        // Root should have one child: "a"
        let root_cc = u16::from_le_bytes([buf[0], buf[1]]);
        assert_eq!(root_cc, 1);
        let mut cursor = 2usize;
        let name_len = u16::from_le_bytes([buf[cursor], buf[cursor + 1]]) as usize;
        cursor += 2;
        assert_eq!(&buf[cursor..cursor + name_len], b"a");
    }

    #[test]
    fn serialize_inode_bits_correct() {
        // Verify the dentry layout for a StagedInode
        let tree = build(&[Action::Stage {
            path: "/f".into(),
            ino: 42,
            pre: Target::Absence,
        }]);
        let buf = tree.serialize();
        // Skip: root child_count(2) + name_len(2) + name(1) = offset 5
        assert_eq!(buf[5], 1); // kind=STAGED_INODE
        let ino = u32::from_le_bytes(buf[6..10].try_into().unwrap());
        assert_eq!(ino, 42);
    }

    #[test]
    fn serialize_link_bits_correct() {
        // Verify the dentry layout for a Redirect
        let tree = build(&[Action::Rename {
            src: "/src".into(),
            dst: "/dst".into(),
            src_pre: Target::BasePath("/src".into()),
            dst_pre: Target::Absence,
        }]);
        let buf = tree.serialize();
        // Find the "dst" node (children sorted: "dst" comes before "src")
        let mut cursor = 2usize; // skip root child_count
        let name_len = u16::from_le_bytes([buf[cursor], buf[cursor + 1]]) as usize;
        cursor += 2;
        assert_eq!(&buf[cursor..cursor + name_len], b"dst");
        cursor += name_len;
        assert_eq!(buf[cursor], 2); // kind=REDIRECT
        cursor += 1;

        // Trailing base_path
        let base_len = u16::from_le_bytes([buf[cursor], buf[cursor + 1]]) as usize;
        cursor += 2;
        assert_eq!(&buf[cursor..cursor + base_len], b"/src");
    }

    // serialize_unset_file_omitted test removed — File(Target::Unset) no longer
    // exists.  File roundtrips remove the node entirely from the tree.

    #[test]
    fn serialize_passthrough_dir_val_zero() {
        // A passthrough dir with children should serialize kind=0
        // but still emit the subtree.
        let mut tree = DirTree::new();
        let mut sub = DirTree::new();
        sub.nodes.insert(
            "child".to_string(),
            DirNode {
                start: None,
                end: Some(Target::StagedFile(1)),
                children: DirTree::new(),
            },
        );
        tree.nodes.insert(
            "dir".to_string(),
            DirNode {
                start: None,
                end: None,
                children: sub,
            },
        );
        let buf = tree.serialize();
        let mut cursor = 0usize;
        // root child_count = 1
        assert_eq!(u16::from_le_bytes([buf[cursor], buf[cursor + 1]]), 1);
        cursor += 2;
        // name = "dir"
        let nlen = u16::from_le_bytes([buf[cursor], buf[cursor + 1]]) as usize;
        cursor += 2;
        assert_eq!(&buf[cursor..cursor + nlen], b"dir");
        cursor += nlen;
        // kind = 2, path_len=0 (passthrough scaffold)
        assert_eq!(
            buf[cursor], 2,
            "passthrough dir should have kind=2 (redirect scaffold)"
        );
        cursor += 1;
        assert_eq!(
            u16::from_le_bytes([buf[cursor], buf[cursor + 1]]),
            0,
            "passthrough dir should have path_len=0"
        );
        cursor += 2;
        // subtree child_count = 1 (the child)
        assert_eq!(u16::from_le_bytes([buf[cursor], buf[cursor + 1]]), 1);
    }

    #[test]
    fn serialize_negative_dentry_dir() {
        // delete on a base-only dir produces a negative dentry.
        let tree = build(&[delete("/d")]);
        let buf = tree.serialize();
        // Skip root child_count(2) + name_len(2) + name(1) = offset 5
        assert_eq!(buf[5], 3); // kind=None/negative
    }

    #[test]
    fn serialize_negative_dentry_symlink() {
        // delete on a base-only symlink produces a negative dentry.
        let tree = build(&[Action::Delete {
            path: "/s".into(),
            pre: Target::Absence,
        }]);
        let buf = tree.serialize();
        assert_eq!(buf[5], 3); // kind=None/negative
    }

    #[test]
    fn serialize_after_roundtrip_rename_includes_tombstone() {
        // Roundtrip rename (a→tmp→a) removes the file node at /a,
        // but /tmp gets a tombstone which appears in serialization.
        let tree = build(&[rename("/tmp", "/a"), rename_from("/a", "/tmp", "/a")]);
        assert_eq!(tree.len(), 1);
        let buf = tree.serialize();
        // Root child_count = 1 (tombstone at /tmp)
        assert_eq!(u16::from_le_bytes([buf[0], buf[1]]), 1);
    }

    #[test]
    fn roundtrip_rename_dir_preserves_subtree() {
        // Rename dir with children back to original — children should survive.
        // get() returns None for passthrough, so /a won't appear.
        // /tmp gets a tombstone (rename always tombstones at source).
        let tree = build(&[
            add("/a/child", 1),
            rename("/tmp", "/a"),
            rename_from("/a", "/tmp", "/a"),
        ]);
        assert_eq!(tree.len(), 2, "child + tombstone at /tmp");
        assert!(
            matches!(tree.get("/a/child"), Some(Target::StagedFile(1))),
            "/a/child should survive roundtrip: {:?}",
            tree
        );
        assert!(
            matches!(tree.get("/tmp"), Some(Target::Absence)),
            "/tmp should be tombstoned: {:?}",
            tree
        );
    }

    #[test]
    fn serialize_deeply_nested_passthrough_dirs() {
        // /a/b/c/file — three levels of passthrough scaffolds.
        // Each intermediate dir must serialize as tag=2, path_len=0
        // so the kernel's travel_inject_entry skips them correctly.
        let tree = build(&[add("/a/b/c/file", 42)]);
        let buf = tree.serialize();
        let mut cursor = 0usize;

        // Root child_count = 1
        assert_eq!(u16::from_le_bytes([buf[cursor], buf[cursor + 1]]), 1);
        cursor += 2;

        // For each passthrough dir (a, b, c): name + tag=2 + path_len=0 + child_count=1
        for name in &["a", "b", "c"] {
            let nlen = u16::from_le_bytes([buf[cursor], buf[cursor + 1]]) as usize;
            cursor += 2;
            assert_eq!(&buf[cursor..cursor + nlen], name.as_bytes());
            cursor += nlen;
            assert_eq!(buf[cursor], 2, "{name}: tag should be 2 (PATH/passthrough)");
            cursor += 1;
            assert_eq!(
                u16::from_le_bytes([buf[cursor], buf[cursor + 1]]),
                0,
                "{name}: path_len should be 0"
            );
            cursor += 2;
            assert_eq!(
                u16::from_le_bytes([buf[cursor], buf[cursor + 1]]),
                1,
                "{name}: child_count should be 1"
            );
            cursor += 2;
        }

        // Leaf file: name + tag=1 (INODE) + ino=42 + child_count=0
        let nlen = u16::from_le_bytes([buf[cursor], buf[cursor + 1]]) as usize;
        cursor += 2;
        assert_eq!(&buf[cursor..cursor + nlen], b"file");
        cursor += nlen;
        assert_eq!(buf[cursor], 1); // tag=INODE
        cursor += 1;
        assert_eq!(
            u32::from_le_bytes(buf[cursor..cursor + 4].try_into().unwrap()),
            42
        );
        cursor += 4;
        assert_eq!(u16::from_le_bytes([buf[cursor], buf[cursor + 1]]), 0);
        cursor += 2;

        assert_eq!(cursor, buf.len());
    }

    #[test]
    fn roundtrip_rename_with_sibling_changes() {
        // Roundtrip rename (a→tmp→a) should remove the roundtripped file
        // but leave other staged changes intact. /tmp gets tombstoned.
        let tree = build(&[
            add("/other", 1),
            rename("/tmp", "/a"),
            rename_from("/a", "/tmp", "/a"),
        ]);
        // "a" removed (roundtrip), "other" survives, "tmp" tombstoned
        assert!(
            tree.nodes.get("a").is_none(),
            "roundtrip file should be removed"
        );
        assert!(
            matches!(tree.get("/other"), Some(Target::StagedFile(1))),
            "sibling should survive: {:?}",
            tree
        );
        assert!(
            matches!(tree.get("/tmp"), Some(Target::Absence)),
            "/tmp should be tombstoned: {:?}",
            tree
        );

        let buf = tree.serialize();
        let mut cursor = 0usize;
        // Root child_count = 2 ("other" + "tmp")
        assert_eq!(u16::from_le_bytes([buf[cursor], buf[cursor + 1]]), 2);
        cursor += 2;
        let nlen = u16::from_le_bytes([buf[cursor], buf[cursor + 1]]) as usize;
        cursor += 2;
        assert_eq!(&buf[cursor..cursor + nlen], b"other");
    }

    #[test]
    #[should_panic(expected = "base_path too long")]
    fn serialize_rejects_oversized_src() {
        let mut tree = DirTree::new();
        tree.nodes.insert(
            "link".to_string(),
            DirNode {
                start: None,
                end: Some(Target::BasePath("a".repeat(u16::MAX as usize + 1))),
                children: DirTree::new(),
            },
        );
        tree.serialize();
    }

    // ── Notes are no-ops for the tree ─────────────────────────────────

    #[test]
    fn notes_interleaved_with_actions_do_not_affect_tree() {
        let with_notes = DirTree::build(std::iter::once(Segment {
            from: 0,
            records: vec![
                Record::Note(Note::Block {
                    path: "/etc/passwd".into(),
                    op: Op::Write,
                }),
                Record::Action(Action::Stage {
                    path: "/a".into(),
                    ino: 1,
                    pre: Target::Absence,
                }),
                Record::Note(Note::Block {
                    path: "/etc/shadow".into(),
                    op: Op::Write,
                }),
                Record::Action(Action::Delete {
                    path: "/b".into(),
                    pre: Target::Absence,
                }),
                Record::Note(Note::Block {
                    path: "/etc/group".into(),
                    op: Op::Write,
                }),
            ],
        }));
        let without_notes = build(&[
            Action::Stage {
                path: "/a".into(),
                ino: 1,
                pre: Target::Absence,
            },
            Action::Delete {
                path: "/b".into(),
                pre: Target::Absence,
            },
        ]);
        assert_eq!(with_notes, without_notes);
    }
}

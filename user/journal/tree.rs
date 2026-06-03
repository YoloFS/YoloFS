// yolo CLI — journal/tree.rs
//
// Dir tree builder. Applies journal records sequentially to build a tree
// representing the overlay state (the in-kernel dirent table).
//
// Two rules govern tombstones (Target::Tombstone entries):
//   1. Rename vacate: when R moves a node away, place a tombstone at the
//      vacated position.
//   2. Delete always tombstones: D on any node → tombstone.
//
// Passthrough (scaffold) dirs that exist only to provide a path to deeper
// nodes carry a passthrough target (`Target::Passthrough`).

use std::collections::HashMap;

use super::types::*;

/// A node in the dir tree.
///
/// Every node carries a `Target` describing the overlay state at that path
/// and a `children` subtree. Leaf nodes (files, symlinks) simply have an
/// empty children map. Passthrough dirs (scaffolds with no staged change)
/// carry `Target::Passthrough` and exist only to provide a path to deeper
/// nodes.
#[derive(Debug, Clone, PartialEq)]
pub struct DirNode {
    pub target: Target,
    pub children: DirTree,
}

/// A directory tree mapping child names to nodes.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct DirTree {
    pub nodes: HashMap<String, DirNode>,
}

impl DirTree {
    fn new() -> Self {
        Self {
            nodes: HashMap::new(),
        }
    }

    /// Apply a single journal action to the tree (consumes the action).
    fn apply(&mut self, action: Action) {
        match action {
            Action::Stage { path, ino } => {
                let target = Target::StagedFile(ino);
                self.set_target(path, target);
            }
            Action::Delete { path } => {
                self.set_target(path, Target::Tombstone);
            }
            Action::Rename { dst, src } => {
                self.apply_rename(dst, src);
            }
        }
    }

    /// Build a tree from owned segments.
    pub fn build(segments: impl IntoIterator<Item = Segment>) -> Self {
        let mut tree = Self::new();
        for seg in segments {
            for record in seg.records {
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
    pub fn into_plan(&self) -> super::plan::CommitPlan {
        super::plan::into_plan(self)
    }

    /// Number of entries (files, dirs with metadata, negative entries) in the tree.
    /// Passthrough entries are excluded — they represent no staged change.
    pub fn len(&self) -> usize {
        self.nodes
            .values()
            .map(|n| {
                let own = if matches!(n.target, Target::Passthrough) {
                    0
                } else {
                    1
                };
                own + n.children.len()
            })
            .sum()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Visit each (full-path, target) pair by reference.
    pub fn for_each<F: FnMut(&str, &Target)>(&self, mut f: F) {
        self.visit_targets(&mut f, &mut String::new());
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

    /// Look up a target by its full path (e.g. "/dir/file").
    /// Returns `None` if the path is not in the tree or is a passthrough.
    pub fn get(&self, path: &str) -> Option<&Target> {
        self.get_node(path)
            .map(|n| &n.target)
            .filter(|t| !matches!(t, Target::Passthrough))
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
    /// Passthrough dirs use tag=2, path_len=0.
    /// File nodes always have child_count=0.
    pub fn serialize(&self) -> Vec<u8> {
        let mut buf = Vec::new();
        self.serialize_into(&mut buf);
        buf
    }

    fn serialize_into(&self, buf: &mut Vec<u8>) {
        // Collect children sorted by name, skipping empty passthrough dirs.
        let mut children: Vec<(&str, &DirNode)> = self
            .nodes
            .iter()
            .filter(|(_, node)| {
                !(matches!(node.target, Target::Passthrough) && node.children.nodes.is_empty())
            })
            .map(|(name, node)| (name.as_str(), node))
            .collect();
        children.sort_by_key(|(name, _)| *name);

        let count: u16 = children.len().try_into().expect("too many children");
        buf.extend_from_slice(&count.to_le_bytes());

        for (name, node) in children {
            let name_bytes = name.as_bytes();
            let name_len: u16 = name_bytes.len().try_into().expect("name too long");
            buf.extend_from_slice(&name_len.to_le_bytes());
            buf.extend_from_slice(name_bytes);

            Self::serialize_target(&node.target, buf);
            node.children.serialize_into(buf);
        }
    }

    fn serialize_target(target: &Target, buf: &mut Vec<u8>) {
        match target {
            Target::Passthrough => {
                // Passthrough: tag=2 (Path), path_len=0
                buf.push(2);
                buf.extend_from_slice(&0u16.to_le_bytes()); // path_len = 0
            }
            Target::StagedFile(ino) => {
                assert!(*ino > 0, "inode ino must be non-zero");
                buf.push(1);
                buf.extend_from_slice(&ino.to_le_bytes());
            }
            Target::BasePath(src) => {
                buf.push(2);
                let bp = src.as_bytes();
                let bp_len: u16 = bp.len().try_into().expect("base_path too long");
                buf.extend_from_slice(&bp_len.to_le_bytes());
                buf.extend_from_slice(bp);
            }
            Target::Tombstone => {
                buf.push(3);
            }
        }
    }

    // ── Internal helpers ──────────────────────────────────────────────

    /// Walk to a path (owned), creating intermediate passthrough Dir nodes as
    /// needed.  Extracts the leaf name from the path in-place via `drain`,
    /// avoiding allocation for the leaf component.
    fn walk_or_create_parent(&mut self, mut path: String) -> Option<(&mut DirTree, String)> {
        let last_slash = path.rfind('/')?;
        if last_slash + 1 >= path.len() {
            return None;
        }
        let mut current = self;
        for part in path[..last_slash].split('/').filter(|s| !s.is_empty()) {
            if !current.nodes.contains_key(part) {
                current.nodes.insert(
                    part.to_string(),
                    DirNode {
                        target: Target::Passthrough,
                        children: DirTree::new(),
                    },
                );
            }
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

    /// Set a target at the given path (owned), preserving any existing subtree.
    fn set_target(&mut self, path: String, target: Target) {
        let Some((parent, name)) = self.walk_or_create_parent(path) else {
            return;
        };
        match parent.nodes.get_mut(name.as_str()) {
            Some(node) => node.target = target,
            None => {
                parent.nodes.insert(
                    name,
                    DirNode {
                        target,
                        children: DirTree::new(),
                    },
                );
            }
        }
    }

    /// Resolve an overlay path to the base filesystem path that backs it,
    /// following the deepest `BasePath` ancestor redirect. With no redirecting
    /// ancestor the path is returned unchanged. E.g. with `/dir → BasePath("/a")`,
    /// `/dir/f` resolves to `/a/f`.
    ///
    /// This is how a renamed base directory's children stay anchored to their
    /// (immutable) base location: the journal records overlay paths, but the
    /// base content lives under the pre-rename path.
    pub fn resolve_base_path(&self, path: &str) -> String {
        let parts: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
        let mut current = self;
        let mut base_at: Option<(usize, &str)> = None;

        for (i, &part) in parts.iter().enumerate() {
            match current.nodes.get(part) {
                Some(node) => {
                    if let Target::BasePath(base) = &node.target {
                        base_at = Some((i, base.as_str()));
                    }
                    current = &node.children;
                }
                None => break,
            }
        }

        match base_at {
            Some((i, base)) => {
                let suffix = &parts[i + 1..];
                if suffix.is_empty() {
                    base.to_string()
                } else {
                    format!("{}/{}", base, suffix.join("/"))
                }
            }
            None => path.to_string(),
        }
    }

    /// Detach a node and resolve its overlay path to a base filesystem path
    /// (see [`resolve_base_path`](Self::resolve_base_path)).
    fn detach_resolved(&mut self, path: &str) -> (Option<DirNode>, String) {
        let resolved = self.resolve_base_path(path);
        let node = self.detach(path);
        (node, resolved)
    }

    /// Apply R rename (owned paths).
    fn apply_rename(&mut self, dst_path: String, src_path: String) {
        if dst_path == src_path {
            return;
        }

        // Detach and resolve in one walk: detach the source node and
        // resolve the overlay path to a base filesystem path.
        let (src_node, resolved_src) = self.detach_resolved(&src_path);

        // Build the node to place at destination
        let dst_node = match src_node {
            Some(mut node) => {
                // Source existed — move it
                if matches!(node.target, Target::Passthrough) {
                    // Passthrough dir being explicitly renamed — create redirect
                    node.target = Target::BasePath(resolved_src);
                }
                node
            }
            None => {
                // No source node — base-only file. Create redirect.
                DirNode {
                    target: Target::BasePath(resolved_src),
                    children: DirTree::new(),
                }
            }
        };

        // Always tombstone at source
        self.set_target(src_path, Target::Tombstone);

        // Roundtrip collapse: if dest ends up as a redirect pointing to itself,
        // the rename chain was a no-op (e.g. a→b→a). Replace with passthrough.
        let is_roundtrip = match &dst_node.target {
            Target::BasePath(src) => src == &dst_path,
            _ => false,
        };

        // Place at destination (handle directory merging)
        let Some((parent, name)) = self.walk_or_create_parent(dst_path) else {
            return;
        };
        if is_roundtrip {
            if dst_node.children.nodes.is_empty() {
                // no-op file — remove entirely (clears any tombstone placed earlier)
                parent.nodes.remove(name.as_str());
            } else {
                // Roundtrip dir — preserve children, set passthrough
                parent.nodes.insert(
                    name,
                    DirNode {
                        target: Target::Passthrough,
                        children: dst_node.children,
                    },
                );
            }
        } else {
            parent.nodes.insert(name, dst_node);
        }
    }

    /// Detach a node from the tree, returning it. Returns None if not found.
    fn detach(&mut self, path: &str) -> Option<DirNode> {
        let (parent, name) = self.walk_to_parent(path)?;
        parent.nodes.remove(name)
    }

    /// Walk the tree by reference, calling `f` for each (path, target).
    fn visit_targets<F: FnMut(&str, &Target)>(&self, f: &mut F, prefix: &mut String) {
        for (name, node) in &self.nodes {
            let path_len = prefix.len();
            prefix.push('/');
            prefix.push_str(name);

            if !matches!(node.target, Target::Passthrough) {
                f(prefix, &node.target);
            }
            node.children.visit_targets(f, prefix);

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

    // ── Delete ─────────────────────────────────────────────────────────

    #[test]
    fn add_then_delete_tombstones() {
        // Delete always tombstones — no cancel even for staged entries.
        let tree = build(&[add("/a", 1), delete("/a")]);
        assert_eq!(tree.len(), 1);
        assert!(matches!(tree.get("/a"), Some(Target::Tombstone)));
    }

    #[test]
    fn modify_then_delete_tombstone() {
        let tree = build(&[add("/a", 1), delete("/a")]);
        assert_eq!(tree.len(), 1);
        assert!(matches!(tree.get("/a"), Some(Target::Tombstone)));
    }

    #[test]
    fn delete_base_only_tombstone() {
        let tree = build(&[delete("/a")]);
        assert_eq!(tree.len(), 1);
        assert!(matches!(tree.get("/a"), Some(Target::Tombstone)));
    }

    // ── Rename (R) ────────────────────────────────────────────────────

    #[test]
    fn rename_added_file() {
        let tree = build(&[add("/a", 1), rename("/b", "/a")]);
        // A + R: rename always tombstones at source.
        // Destination gets the Inode.
        assert_eq!(tree.len(), 2);
        assert!(matches!(tree.get("/a"), Some(Target::Tombstone)));
        assert!(matches!(tree.get("/b"), Some(Target::StagedFile(1))));
    }

    #[test]
    fn rename_base_only_file() {
        let tree = build(&[rename("/b", "/a")]);
        // Base-only: Link at /b, Tombstone at /a
        assert_eq!(tree.len(), 2);
        assert!(matches!(tree.get("/a"), Some(Target::Tombstone)));
        assert!(matches!(tree.get("/b"), Some(Target::BasePath(from)) if from == "/a"));
    }

    // ── Rename chain ──────────────────────────────────────────────────

    #[test]
    fn rename_chain() {
        let tree = build(&[rename("/b", "/a"), rename("/c", "/b")]);
        // a→b→c: Tombstone at /a, tombstone at /b (always tombstones at source), Link at /c
        assert_eq!(tree.len(), 3);
        assert!(matches!(tree.get("/a"), Some(Target::Tombstone)));
        assert!(matches!(tree.get("/b"), Some(Target::Tombstone)));
        assert!(matches!(tree.get("/c"), Some(Target::BasePath(from)) if from == "/a"));
    }

    // ── Source resolution (BasePath must reference immutable base) ────

    #[test]
    fn rename_through_redirect_resolves_to_base() {
        // mv /a /dir, then mv /dir/f /x.
        // /dir/f is an overlay path; resolved source should be /a/f (base path).
        let tree = build(&[rename("/dir", "/a"), rename("/x", "/dir/f")]);
        assert!(matches!(tree.get("/x"), Some(Target::BasePath(src)) if src == "/a/f"));
    }

    #[test]
    fn rename_through_nested_redirect_uses_deepest() {
        // mv /a /dir, then mv /b /dir/sub, then mv /dir/sub/f /x.
        // /dir → BasePath("/a"), /dir/sub → BasePath("/b").
        // /dir/sub/f should resolve through /dir/sub (deepest) to /b/f, not /a/sub/f.
        let tree = build(&[
            rename("/dir", "/a"),
            rename("/dir/sub", "/b"),
            rename("/x", "/dir/sub/f"),
        ]);
        assert!(matches!(tree.get("/x"), Some(Target::BasePath(src)) if src == "/b/f"));
    }

    #[test]
    fn rename_no_redirect_keeps_base_path() {
        // mv /a /b — no redirect ancestor, source is already a base path.
        let tree = build(&[rename("/b", "/a")]);
        assert!(matches!(tree.get("/b"), Some(Target::BasePath(src)) if src == "/a"));
    }

    // ── Rename then delete ────────────────────────────────────────────

    #[test]
    fn rename_then_delete_base_file() {
        let tree = build(&[rename("/b", "/a"), delete("/b")]);
        // R(/b, /a): Link at /b, Tombstone at /a
        // D(/b): always tombstones → Tombstone at /b
        // Result: Tombstone at /a and Tombstone at /b
        assert_eq!(tree.len(), 2);
        assert!(matches!(tree.get("/a"), Some(Target::Tombstone)));
        assert!(matches!(tree.get("/b"), Some(Target::Tombstone)));
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
        assert!(matches!(tree.get("/dir"), Some(Target::Tombstone)));
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
        // Tree: Inode(ino=5) at /b, Tombstone at /a.
        let tree = build(&[rename("/b", "/a"), add("/b", 5)]);
        assert_eq!(tree.len(), 2);
        assert!(matches!(tree.get("/a"), Some(Target::Tombstone)));
        assert!(matches!(tree.get("/b"), Some(Target::StagedFile(5))));
    }

    // ── Rename over tombstone ─────────────────────────────────────────

    #[test]
    fn rename_over_tombstone() {
        // Delete /b (creates tombstone), then rename /a → /b.
        // The rename replaces the tombstone with a Link.
        let tree = build(&[delete("/b"), rename("/b", "/a")]);
        assert_eq!(tree.len(), 2);
        assert!(matches!(tree.get("/a"), Some(Target::Tombstone)));
        assert!(matches!(tree.get("/b"), Some(Target::BasePath(from)) if from == "/a"));
    }

    // ── Add then rename (staged rename) ───────────────────────────────

    #[test]
    fn add_then_rename_preserves_inode() {
        // A(/a, ino=1) then R(/b, /a): staged file moved, inode preserved.
        let tree = build(&[add("/a", 1), rename("/b", "/a")]);
        // Rename always tombstones at source.
        // Inode moved to /b.
        assert_eq!(tree.len(), 2);
        assert!(matches!(tree.get("/a"), Some(Target::Tombstone)));
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
        // A over Tombstone: the new inode replaces the tombstone.
        assert!(matches!(tree.get("/a"), Some(Target::StagedFile(2))));
    }

    // ── delete_dir ────────────────────────────────────────────────────

    #[test]
    fn delete_dir_base_only_tombstone() {
        let tree = build(&[delete("/d")]);
        assert_eq!(tree.len(), 1);
        assert!(matches!(tree.get("/d"), Some(Target::Tombstone)));
    }

    #[test]
    fn add_dir_then_delete_dir_tombstones() {
        // Delete always tombstones — no cancel even for staged dirs.
        let tree = build(&[add("/d", 1), delete("/d")]);
        assert_eq!(tree.len(), 1);
        assert!(matches!(tree.get("/d"), Some(Target::Tombstone)));
    }

    #[test]
    fn delete_dir_with_children() {
        // Deleting a dir tombstones it; children remain in the subtree.
        let tree = build(&[add("/d", 1), add("/d/f1", 2), add("/d/f2", 3), delete("/d")]);
        assert_eq!(tree.len(), 3);
        assert!(matches!(tree.get("/d"), Some(Target::Tombstone)));
        assert!(matches!(tree.get("/d/f1"), Some(Target::StagedFile(2))));
        assert!(matches!(tree.get("/d/f2"), Some(Target::StagedFile(3))));
    }

    #[test]
    fn delete_dir_preserves_children() {
        let tree = build(&[
            add("/d", 1),
            add("/d/f", 2),
            Action::Delete { path: "/d".into() },
        ]);
        // Delete always tombstones. Children remain in the subtree.
        assert_eq!(tree.len(), 2);
        assert!(matches!(tree.get("/d"), Some(Target::Tombstone)));
        assert!(matches!(tree.get("/d/f"), Some(Target::StagedFile(2))));
    }

    #[test]
    fn delete_base_dir_then_add_child() {
        // Delete a base directory, then add a file under it.
        // The tombstone should be a Dir node so walk_to_parent succeeds.
        let tree = build(&[delete("/d"), add("/d/f1", 1)]);
        assert!(matches!(tree.get("/d"), Some(Target::Tombstone)));
        assert!(matches!(tree.get("/d/f1"), Some(Target::StagedFile(1))));
    }

    // ── Delete intermediate directory ─────────────────────────────────

    #[test]
    fn delete_intermediate_dir_creates_tombstone() {
        // Add /a/b/c/file — creates intermediate /a, /a/b, /a/b/c.
        // Delete /a/b → should create Tombstone for the intermediate dir.
        let tree = build(&[add("/a/b/c/file", 1), delete("/a/b")]);
        // /a/b was intermediate (Dir(passthrough,..)) → treated as base → negative dentry.
        assert!(
            matches!(tree.get("/a/b"), Some(Target::Tombstone)),
            "intermediate dir should get Tombstone: {:?}",
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
        let tree = build(&[rename("/tmp", "/a"), rename("/a", "/tmp")]);
        assert_eq!(tree.len(), 1, "/tmp tombstone is the only staged change");
        // File roundtrip — node at /a removed entirely from tree.
        assert!(
            tree.nodes.get("a").is_none(),
            "expected file roundtrip to remove node, got {:?}",
            tree.nodes.get("a")
        );
        assert!(
            matches!(tree.get("/tmp"), Some(Target::Tombstone)),
            "/tmp should be tombstoned: {:?}",
            tree
        );
    }

    #[test]
    fn roundtrip_rename_dir_removed() {
        // Dir roundtrip (a→tmp→a) — base-only renames with empty
        // children are removed (same as file roundtrip).
        // /tmp gets a tombstone (rename always tombstones at source).
        let tree = build(&[rename("/tmp", "/a"), rename("/a", "/tmp")]);
        assert_eq!(tree.len(), 1, "/tmp tombstone is the only staged change");
        assert!(
            tree.nodes.get("a").is_none(),
            "base-only roundtrip with no children should remove node, got {:?}",
            tree.nodes.get("a")
        );
        assert!(
            matches!(tree.get("/tmp"), Some(Target::Tombstone)),
            "/tmp should be tombstoned: {:?}",
            tree
        );
    }

    #[test]
    fn three_cycle_swap() {
        // a→tmp, b→a, tmp→b: swaps a and b via tmp.
        let tree = build(&[
            rename("/tmp", "/a"),
            rename("/a", "/b"),
            rename("/b", "/tmp"),
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
        let tree = build(&[rename("/b", "/a"), rename("/c", "/b"), rename("/a", "/c")]);
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
            matches!(tree.get("/b"), Some(Target::Tombstone)),
            "/b should be tombstoned: {:?}",
            tree
        );
        assert!(
            matches!(tree.get("/c"), Some(Target::Tombstone)),
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
        assert!(matches!(tree.get("/old"), Some(Target::Tombstone)));
        assert!(matches!(tree.get("/new"), Some(Target::BasePath(_))));
    }

    #[test]
    fn add_then_delete_symlink_tombstones() {
        // Delete always tombstones — no cancel even for staged symlinks.
        let tree = build(&[
            add("/link", 1),
            Action::Delete {
                path: "/link".into(),
            },
        ]);
        assert_eq!(tree.len(), 1);
        assert!(matches!(tree.get("/link"), Some(Target::Tombstone)));
    }

    // ── Base-only directory rename ────────────────────────────────────

    #[test]
    fn rename_base_only_dir() {
        let tree = build(&[rename("/new", "/old")]);
        assert_eq!(tree.len(), 2);
        assert!(matches!(tree.get("/old"), Some(Target::Tombstone)));
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
            },
        ]);
        // Rename always tombstones at source
        assert!(matches!(tree.get("/old"), Some(Target::Tombstone)));
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
            matches!(tree.get("/new/f1"), Some(Target::Tombstone)),
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
            matches!(tree.get("/a/b"), Some(Target::Tombstone)),
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
        // /b existed in base, so deleting the Link must leave a Tombstone
        // to hide the base content. Without it, base /b reappears.
        let cs = build(&[
            Action::Rename {
                src: "/a".into(),
                dst: "/b".into(),
            },
            Action::Delete { path: "/b".into() },
        ]);
        // Both /a and /b should be tombstoned.
        assert!(
            matches!(cs.get("/a"), Some(Target::Tombstone)),
            "/a should be Tombstone: {:?}",
            cs
        );
        assert!(
            matches!(cs.get("/b"), Some(Target::Tombstone)),
            "/b should be Tombstone (base content): {:?}",
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
            },
            Action::Delete {
                path: "/old".into(),
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
    fn serialize_passthrough_dir_empty_subtree_omitted() {
        // Create a tree with a passthrough dir that has an empty subtree
        let mut tree = DirTree::new();
        tree.nodes.insert(
            "empty".to_string(),
            DirNode {
                target: Target::Passthrough,
                children: DirTree::new(),
            },
        );
        let buf = tree.serialize();
        // Should produce just child_count=0 (the empty passthrough dir is skipped)
        assert_eq!(buf, vec![0x00, 0x00]);
    }

    #[test]
    fn serialize_stale_intermediates_after_cancel() {
        // Add a deeply nested file then delete it. Delete always tombstones,
        // so the leaf gets a tombstone. Passthrough Dir intermediates remain.
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
                target: Target::StagedFile(1),
                children: DirTree::new(),
            },
        );
        tree.nodes.insert(
            "dir".to_string(),
            DirNode {
                target: Target::Passthrough,
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
        let tree = build(&[Action::Delete { path: "/s".into() }]);
        let buf = tree.serialize();
        assert_eq!(buf[5], 3); // kind=None/negative
    }

    #[test]
    fn serialize_after_roundtrip_rename_includes_tombstone() {
        // Roundtrip rename (a→tmp→a) removes the file node at /a,
        // but /tmp gets a tombstone which appears in serialization.
        let tree = build(&[rename("/tmp", "/a"), rename("/a", "/tmp")]);
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
            rename("/a", "/tmp"),
        ]);
        assert_eq!(tree.len(), 2, "child + tombstone at /tmp");
        assert!(
            matches!(tree.get("/a/child"), Some(Target::StagedFile(1))),
            "/a/child should survive roundtrip: {:?}",
            tree
        );
        assert!(
            matches!(tree.get("/tmp"), Some(Target::Tombstone)),
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
        let tree = build(&[add("/other", 1), rename("/tmp", "/a"), rename("/a", "/tmp")]);
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
            matches!(tree.get("/tmp"), Some(Target::Tombstone)),
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
                target: Target::BasePath("a".repeat(u16::MAX as usize + 1)),
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
                }),
                Record::Note(Note::Block {
                    path: "/etc/shadow".into(),
                    op: Op::Write,
                }),
                Record::Action(Action::Delete { path: "/b".into() }),
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
            },
            Action::Delete { path: "/b".into() },
        ]);
        assert_eq!(with_notes, without_notes);
    }
}

// agfs CLI — journal/tree.rs
//
// Dir tree builder. Applies journal records sequentially to build a tree
// representing the overlay state (the in-kernel dirent table).
//
// Three rules govern in_base and negative dentries (Target::None + in_base=true):
//   1. in_base is set by the record tag: A/R → false, M/P → true.
//   2. Negative dentry on vacate: when R/P moves a node away from a base
//      path, place a negative dentry at the vacated position.
//   3. Cancellation: D on a node with in_base=false → remove (cancel).
//      D on a node with in_base=true → negative dentry.
//
// Passthrough (scaffold) dirs that exist only to provide a path to deeper
// nodes carry a passthrough dentry (`Target::Path(None)`) in `Dir(Dentry, DirTree)`.

use std::collections::HashMap;

use super::dentry::{Dentry, Target};
use super::types::*;

/// A node in the dir tree.
///
/// The tree has the following shape:
///
///   DirTree { nodes: { name → DirNode, ... } }
///     DirNode::File(Dentry)          — leaf: a file/symlink with its overlay state
///     DirNode::Dir(Dentry, DirTree)  — branch: a directory with its overlay state
///                                      and a subtree of children
///
/// Every node carries a `Dentry` describing the overlay state at that path.
/// Passthrough dirs (scaffolds with no staged change) carry
/// `Dentry::passthrough()` and exist only to provide a path to deeper nodes.
#[derive(Debug, Clone, PartialEq)]
pub enum DirNode {
    File(Dentry),
    Dir(Dentry, DirTree),
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
            Action::Add { path, dtype, ino } => {
                let is_dir = dtype.unwrap_or(libc::DT_REG) == libc::DT_DIR;
                let dentry = Dentry {
                    target: Target::Inode(ino),
                    in_base: false,
                };
                self.set_dentry(path, dentry, is_dir);
            }
            Action::Modify { path, dtype, ino } => {
                let is_dir = dtype.unwrap_or(libc::DT_REG) == libc::DT_DIR;
                let dentry = Dentry {
                    target: Target::Inode(ino),
                    in_base: true,
                };
                self.set_dentry(path, dentry, is_dir);
            }
            Action::Delete { path, dtype } => {
                let is_dir = dtype.unwrap_or(libc::DT_REG) == libc::DT_DIR;
                self.apply_delete(path, is_dir);
            }
            Action::Rename { dst, src, dtype } => {
                let is_dir = dtype.unwrap_or(libc::DT_REG) == libc::DT_DIR;
                self.apply_rename(dst, src, is_dir, false);
            }
            Action::Replace { dst, src, dtype } => {
                let is_dir = dtype.unwrap_or(libc::DT_REG) == libc::DT_DIR;
                self.apply_rename(dst, src, is_dir, true);
            }
        }
    }

    /// Build a tree from owned segments.
    pub fn build(segments: impl IntoIterator<Item = Segment>) -> Self {
        let mut tree = Self::new();
        for seg in segments {
            for action in seg.records {
                tree.apply(action);
            }
        }
        tree
    }

    /// Number of dentries (files, dirs with metadata, negative dentries) in the tree.
    /// Passthrough entries are excluded — they represent no staged change.
    pub fn len(&self) -> usize {
        self.nodes
            .values()
            .map(|n| match n {
                DirNode::File(d) if d.is_passthrough() => 0,
                DirNode::File(_) => 1,
                DirNode::Dir(d, sub) if d.is_passthrough() => sub.len(),
                DirNode::Dir(_, sub) => 1 + sub.len(),
            })
            .sum()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Visit each (full-path, dentry) pair by reference.
    pub fn for_each<F: FnMut(&str, &Dentry)>(&self, mut f: F) {
        self.visit_dentries(&mut f, &mut String::new());
    }

    /// Return true if any (path, dentry) pair matches the predicate.
    pub fn any<F: FnMut(&str, &Dentry) -> bool>(&self, mut f: F) -> bool {
        let mut found = false;
        self.for_each(|p, d| {
            if !found && f(p, d) {
                found = true;
            }
        });
        found
    }

    /// Look up a dentry by its full path (e.g. "/dir/file").
    /// Returns `None` if the path is not in the tree or is a passthrough.
    pub fn get(&self, path: &str) -> Option<&Dentry> {
        self.get_node(path).map(|n| match n {
            DirNode::File(d) | DirNode::Dir(d, _) => d,
        }).filter(|d| !d.is_passthrough())
    }

    /// Look up a node by its full path (e.g. "/dir/file").
    /// Returns `None` if the path is not in the tree.
    pub fn get_node(&self, path: &str) -> Option<&DirNode> {
        let mut parts = path.split('/').filter(|s| !s.is_empty()).peekable();
        let mut current = self;
        while let Some(part) = parts.next() {
            match current.nodes.get(part) {
                Some(node @ DirNode::File(_)) => {
                    if parts.peek().is_some() {
                        return None; // path continues past a file node
                    }
                    return Some(node);
                }
                Some(node @ DirNode::Dir(_, subtree)) => {
                    if parts.peek().is_none() {
                        return Some(node);
                    }
                    current = subtree;
                }
                None => return None,
            }
        }
        None
    }

    /// Serialize the tree into a contiguous byte buffer for the restore ioctl.
    ///
    /// Wire format (all integers little-endian):
    ///   DirTree      := child_count:le16  DirNode[child_count]
    ///   DirNode      := name_len:le16  name:u8[name_len]
    ///                   Dentry
    ///                   child_count:le16  DirNode[child_count]   (children of this dir)
    ///   Dentry       := tag:u8  in_base:u8  [payload]
    ///                   tag=1 Inode:  ino:le32
    ///                   tag=2 Path:   path_len:le16  path:u8[path_len]
    ///                   tag=3 None:   (no payload)
    ///
    /// Passthrough dirs use tag=2, in_base=1, path_len=0.
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
            .filter(|(_, node)| match node {
                DirNode::Dir(d, sub) if d.is_passthrough() && sub.nodes.is_empty() => false,
                _ => true,
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

            match node {
                DirNode::File(dentry) => {
                    Self::serialize_dentry(dentry, buf);
                    buf.extend_from_slice(&0u16.to_le_bytes()); // child_count = 0
                }
                DirNode::Dir(dentry, subtree) => {
                    Self::serialize_dentry(dentry, buf);
                    subtree.serialize_into(buf);
                }
            }
        }
    }

    fn serialize_dentry(dentry: &Dentry, buf: &mut Vec<u8>) {
        match dentry {
            Dentry { target: Target::Path(None), .. } => {
                // Passthrough: tag=2 (Path), in_base=1, path_len=0
                buf.push(2);
                buf.push(1); // in_base = true
                buf.extend_from_slice(&0u16.to_le_bytes()); // path_len = 0
            }
            Dentry { target: Target::Inode(ino), in_base, .. } => {
                assert!(*ino > 0, "inode ino must be non-zero");
                buf.push(1);
                buf.push(*in_base as u8);
                buf.extend_from_slice(&ino.to_le_bytes());
            }
            Dentry { target: Target::Path(Some(src)), in_base, .. } => {
                buf.push(2);
                buf.push(*in_base as u8);
                let bp = src.as_bytes();
                let bp_len: u16 = bp.len().try_into().expect("base_path too long");
                buf.extend_from_slice(&bp_len.to_le_bytes());
                buf.extend_from_slice(bp);
            }
            Dentry { target: Target::None, in_base, .. } => {
                buf.push(3);
                buf.push(*in_base as u8);
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
                    DirNode::Dir(Dentry::passthrough(), DirTree::new()),
                );
            }
            match current.nodes.get_mut(part).unwrap() {
                DirNode::Dir(_, subtree) => current = subtree,
                DirNode::File(_) => return None,
            }
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
                Some(DirNode::Dir(_, subtree)) => current = subtree,
                _ => return None,
            }
        }
        Some((current, name))
    }

    /// Set a dentry at the given path (owned).
    fn set_dentry(&mut self, path: String, dentry: Dentry, is_dir: bool) {
        let Some((parent, name)) = self.walk_or_create_parent(path) else {
            return;
        };
        if is_dir {
            if let Some(DirNode::Dir(existing_dentry, _)) = parent.nodes.get_mut(name.as_str()) {
                *existing_dentry = dentry;
                return;
            }
            parent.nodes.insert(name, DirNode::Dir(dentry, DirTree::new()));
        } else {
            parent.nodes.insert(name, DirNode::File(dentry));
        }
    }

    /// Apply a D record (owned path).
    fn apply_delete(&mut self, path: String, is_dir: bool) {
        let Some((parent, name)) = self.walk_or_create_parent(path) else {
            return;
        };

        // Check what to do based on current state.
        let needs_tombstone = match parent.nodes.get(name.as_str()) {
            None => true,
            Some(DirNode::File(d) | DirNode::Dir(d, _)) => {
                if d.is_passthrough() { true } else { d.in_base }
            }
        };

        if needs_tombstone {
            Self::place_tombstone_at(parent, name, is_dir);
        } else {
            // in_base=false → cancel (remove)
            parent.nodes.remove(name.as_str());
        }
    }

    /// Place a negative dentry at a path, preserving any existing Dir subtree.
    fn place_tombstone(&mut self, path: String, is_dir: bool) {
        if let Some((parent, name)) = self.walk_or_create_parent(path) {
            Self::place_tombstone_at(parent, name, is_dir);
        }
    }

    /// Place a negative dentry in a parent's node map, preserving any existing Dir subtree.
    fn place_tombstone_at(parent: &mut DirTree, name: String, is_dir: bool) {
        let tombstone = Dentry {
            target: Target::None,
            in_base: true,
        };
        match parent.nodes.get_mut(name.as_str()) {
            Some(DirNode::File(d)) => *d = tombstone,
            Some(DirNode::Dir(d, _)) => *d = tombstone,
            None => {
                let node = if is_dir {
                    DirNode::Dir(tombstone, DirTree::new())
                } else {
                    DirNode::File(tombstone)
                };
                parent.nodes.insert(name, node);
            }
        }
    }

    /// Apply R/P rename (owned paths).
    fn apply_rename(&mut self, dst_path: String, src_path: String, is_dir: bool, dst_in_base: bool) {
        if dst_path == src_path {
            return;
        }

        // Detach source node
        let src_node = self.detach(&src_path);

        // Determine if source position had base content (for tombstone)
        let source_had_base = match &src_node {
            Some(DirNode::File(d) | DirNode::Dir(d, _)) => {
                if d.is_passthrough() { true } else { d.in_base }
            }
            None => true, // no node = base-only
        };

        // Build the node to place at destination
        let dst_node = match src_node {
            Some(mut node) => {
                // Source existed — move it, update in_base
                match &mut node {
                    DirNode::File(d) => d.in_base = dst_in_base,
                    DirNode::Dir(d, _) if d.is_passthrough() => {
                        // Passthrough dir being explicitly renamed — create redirect
                        *d = Dentry {
                            target: Target::Path(Some(src_path.clone())),
                            in_base: dst_in_base,
                        };
                    }
                    DirNode::Dir(d, _) => d.in_base = dst_in_base,
                }
                node
            }
            None => {
                // No source node — base-only file. Create redirect.
                let dentry = Dentry {
                    target: Target::Path(Some(src_path.clone())),
                    in_base: dst_in_base,
                };
                if is_dir {
                    DirNode::Dir(dentry, DirTree::new())
                } else {
                    DirNode::File(dentry)
                }
            }
        };

        // Place tombstone at source if it had base content
        if source_had_base {
            self.place_tombstone(src_path, is_dir);
        }

        // Roundtrip collapse: if dest ends up as a redirect pointing to itself,
        // the rename chain was a no-op (e.g. a→b→a). Replace with passthrough.
        let is_roundtrip = match &dst_node {
            DirNode::File(Dentry { target: Target::Path(Some(src)), .. })
            | DirNode::Dir(Dentry { target: Target::Path(Some(src)), .. }, _) => src == &dst_path,
            _ => false,
        };

        // Place at destination (handle directory merging)
        let Some((parent, name)) = self.walk_or_create_parent(dst_path) else {
            return;
        };
        if is_roundtrip {
            // Roundtrip — the rename chain was a no-op.
            match dst_node {
                DirNode::Dir(_, subtree) => {
                    parent
                        .nodes
                        .insert(name, DirNode::Dir(Dentry::passthrough(), subtree));
                }
                DirNode::File(_) => {
                    // no-op file — remove entirely (clears any tombstone placed earlier)
                    parent.nodes.remove(name.as_str());
                }
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

    /// Walk the tree by reference, calling `f` for each (path, dentry).
    fn visit_dentries<F: FnMut(&str, &Dentry)>(&self, f: &mut F, prefix: &mut String) {
        for (name, node) in &self.nodes {
            let path_len = prefix.len();
            prefix.push('/');
            prefix.push_str(name);

            match node {
                DirNode::File(dentry) if !dentry.is_passthrough() => f(prefix, dentry),
                DirNode::File(_) => {} // skip passthrough file
                DirNode::Dir(dentry, subtree) if !dentry.is_passthrough() => {
                    f(prefix, dentry);
                    subtree.visit_dentries(f, prefix);
                }
                DirNode::Dir(_, subtree) => {
                    subtree.visit_dentries(f, prefix);
                }
            }

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
            records: actions.to_vec(),
        }))
    }

    fn add(path: &str, ino: u32) -> Action {
        Action::Add {
            path: path.into(),
            dtype: Some(libc::DT_REG),
            ino,
        }
    }

    fn add_dir(path: &str, ino: u32) -> Action {
        Action::Add {
            path: path.into(),
            dtype: Some(libc::DT_DIR),
            ino,
        }
    }

    fn modify(path: &str, ino: u32) -> Action {
        Action::Modify {
            path: path.into(),
            dtype: Some(libc::DT_REG),
            ino,
        }
    }

    fn delete(path: &str) -> Action {
        Action::Delete {
            path: path.into(),
            dtype: Some(libc::DT_REG),
        }
    }

    fn delete_dir(path: &str) -> Action {
        Action::Delete {
            path: path.into(),
            dtype: Some(libc::DT_DIR),
        }
    }

    fn rename(dest: &str, src: &str) -> Action {
        Action::Rename {
            dst: dest.into(),
            src: src.into(),
            dtype: Some(libc::DT_REG),
        }
    }

    fn rename_dir(dest: &str, src: &str) -> Action {
        Action::Rename {
            dst: dest.into(),
            src: src.into(),
            dtype: Some(libc::DT_DIR),
        }
    }

    fn replace(dest: &str, src: &str) -> Action {
        Action::Replace {
            dst: dest.into(),
            src: src.into(),
            dtype: Some(libc::DT_REG),
        }
    }

    fn replace_dir(dest: &str, src: &str) -> Action {
        Action::Replace {
            dst: dest.into(),
            src: src.into(),
            dtype: Some(libc::DT_DIR),
        }
    }

    fn add_symlink(path: &str, ino: u32) -> Action {
        Action::Add {
            path: path.into(),
            dtype: Some(libc::DT_LNK),
            ino,
        }
    }

    fn rename_symlink(dest: &str, src: &str) -> Action {
        Action::Rename {
            dst: dest.into(),
            src: src.into(),
            dtype: Some(libc::DT_LNK),
        }
    }

    // ── Basic insert ──────────────────────────────────────────────────

    #[test]
    fn add_single_file() {
        let tree = build(&[add("/a", 1)]);
        assert_eq!(tree.len(), 1);
        assert!(matches!(
            tree.get("/a"),
            Some(Dentry { target: Target::Inode(1), in_base: false, .. })
        ));
    }

    #[test]
    fn modify_single_file() {
        let tree = build(&[modify("/a", 1)]);
        assert_eq!(tree.len(), 1);
        assert!(matches!(
            tree.get("/a"),
            Some(Dentry { target: Target::Inode(1), in_base: true, .. })
        ));
    }

    #[test]
    fn add_nested_file() {
        let tree = build(&[add("/dir/sub/file", 1)]);
        assert_eq!(tree.len(), 1);
        assert!(matches!(
            tree.get("/dir/sub/file"),
            Some(Dentry { target: Target::Inode(1), in_base: false, .. })
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

    // ── Cancellation ──────────────────────────────────────────────────

    #[test]
    fn add_then_delete_cancels() {
        let tree = build(&[add("/a", 1), delete("/a")]);
        assert!(tree.is_empty(), "A + D should cancel: {:?}", tree);
    }

    #[test]
    fn modify_then_delete_tombstone() {
        let tree = build(&[modify("/a", 1), delete("/a")]);
        assert_eq!(tree.len(), 1);
        assert!(matches!(tree.get("/a"), Some(Dentry { target: Target::None, .. })));
    }

    #[test]
    fn delete_base_only_tombstone() {
        let tree = build(&[delete("/a")]);
        assert_eq!(tree.len(), 1);
        assert!(matches!(tree.get("/a"), Some(Dentry { target: Target::None, .. })));
    }

    // ── Rename (R) ────────────────────────────────────────────────────

    #[test]
    fn rename_added_file() {
        let tree = build(&[add("/a", 1), rename("/b", "/a")]);
        // A + R: source was in_base=false → no tombstone at /a.
        // Destination gets the Inode with in_base=false (from R tag).
        assert_eq!(tree.len(), 1);
        assert!(matches!(
            tree.get("/b"),
            Some(Dentry { target: Target::Inode(1), in_base: false, .. })
        ));
    }

    #[test]
    fn rename_base_only_file() {
        let tree = build(&[rename("/b", "/a")]);
        // Base-only: Link at /b, Tombstone at /a
        assert_eq!(tree.len(), 2);
        assert!(matches!(tree.get("/a"), Some(Dentry { target: Target::None, .. })));
        assert!(matches!(tree.get("/b"), Some(Dentry { target: Target::Path(Some(from)), .. }) if from == "/a"));
    }

    // ── Replace (P) ──────────────────────────────────────────────────

    #[test]
    fn replace_base_only() {
        let tree = build(&[replace("/b", "/a")]);
        // P: dest in_base=true, source had base content → Tombstone at /a, Link at /b
        assert_eq!(tree.len(), 2);
        assert!(matches!(tree.get("/a"), Some(Dentry { target: Target::None, .. })));
        assert!(matches!(tree.get("/b"), Some(Dentry { target: Target::Path(Some(from)), .. }) if from == "/a"));
    }

    // ── Rename chain ──────────────────────────────────────────────────

    #[test]
    fn rename_chain() {
        let tree = build(&[rename("/b", "/a"), rename("/c", "/b")]);
        // a→b→c: Tombstone at /a, nothing at /b (not in base), Link at /c
        assert_eq!(tree.len(), 2);
        assert!(matches!(tree.get("/a"), Some(Dentry { target: Target::None, .. })));
        assert!(matches!(tree.get("/c"), Some(Dentry { target: Target::Path(Some(from)), .. }) if from == "/a"));
    }

    // ── Rename then delete ────────────────────────────────────────────

    #[test]
    fn rename_then_delete_base_file() {
        let tree = build(&[rename("/b", "/a"), delete("/b")]);
        // R(/b, /a): Link at /b (in_base=false), Tombstone at /a
        // D(/b): in_base=false → cancel
        // Result: just Tombstone at /a
        assert_eq!(tree.len(), 1);
        assert!(matches!(tree.get("/a"), Some(Dentry { target: Target::None, .. })));
    }

    // ── Directory rename ──────────────────────────────────────────────

    #[test]
    fn dir_rename_moves_children() {
        let tree = build(&[
            add_dir("/dir", 1),
            add("/dir/f1", 2),
            add("/dir/f2", 3),
            rename_dir("/newdir", "/dir"),
        ]);
        assert!(tree.get("/newdir").is_some(), "missing /newdir");
        assert!(tree.get("/newdir/f1").is_some(), "missing /newdir/f1");
        assert!(tree.get("/newdir/f2").is_some(), "missing /newdir/f2");
        assert!(tree.get("/dir").is_none(), "stale /dir");
        assert!(tree.get("/dir/f1").is_none(), "stale /dir/f1");
    }

    // ── Multiple modifies ─────────────────────────────────────────────

    #[test]
    fn multiple_modifies_last_wins() {
        let tree = build(&[modify("/a", 1), modify("/a", 2), modify("/a", 3)]);
        assert_eq!(tree.len(), 1);
        assert!(matches!(
            tree.get("/a"),
            Some(Dentry { target: Target::Inode(3), in_base: true, .. })
        ));
    }

    // ── Rename + modify at dest ───────────────────────────────────────

    #[test]
    fn rename_then_modify_at_dest() {
        // R(/b, /a) then M(/b, ino=5): base file renamed, then modified at dest.
        // Tree: Link at /b replaced by Inode(ino=5, in_base=true), Tombstone at /a.
        let tree = build(&[rename("/b", "/a"), modify("/b", 5)]);
        assert_eq!(tree.len(), 2);
        assert!(matches!(tree.get("/a"), Some(Dentry { target: Target::None, .. })));
        assert!(matches!(
            tree.get("/b"),
            Some(Dentry { target: Target::Inode(5), in_base: true, .. })
        ));
    }

    #[test]
    fn replace_then_modify_at_dest() {
        // P(/b, /a) then M(/b, ino=5): overwrites base /b with renamed /a, then modified.
        let tree = build(&[replace("/b", "/a"), modify("/b", 5)]);
        assert_eq!(tree.len(), 2);
        assert!(matches!(tree.get("/a"), Some(Dentry { target: Target::None, .. })));
        assert!(matches!(
            tree.get("/b"),
            Some(Dentry { target: Target::Inode(5), in_base: true, .. })
        ));
    }

    // ── Rename over tombstone ─────────────────────────────────────────

    #[test]
    fn rename_over_tombstone() {
        // Delete /b (creates tombstone), then rename /a → /b.
        // The rename replaces the tombstone with a Link.
        let tree = build(&[delete("/b"), rename("/b", "/a")]);
        assert_eq!(tree.len(), 2);
        assert!(matches!(tree.get("/a"), Some(Dentry { target: Target::None, .. })));
        assert!(matches!(tree.get("/b"), Some(Dentry { target: Target::Path(Some(from)), .. }) if from == "/a"));
    }

    // ── Add then rename (staged rename) ───────────────────────────────

    #[test]
    fn add_then_rename_preserves_inode() {
        // A(/a, ino=1) then R(/b, /a): staged file moved, inode preserved.
        let tree = build(&[add("/a", 1), rename("/b", "/a")]);
        // Source was in_base=false → no tombstone at /a.
        // Inode moved to /b with in_base=false.
        assert_eq!(tree.len(), 1);
        assert!(matches!(
            tree.get("/b"),
            Some(Dentry { target: Target::Inode(1), in_base: false, .. })
        ));
    }

    // ── Create-delete-recreate ────────────────────────────────────────

    #[test]
    fn add_delete_add_same_path() {
        // A + D cancels, then A creates fresh.
        let tree = build(&[add("/a", 1), delete("/a"), add("/a", 2)]);
        assert_eq!(tree.len(), 1);
        assert!(matches!(
            tree.get("/a"),
            Some(Dentry { target: Target::Inode(2), in_base: false, .. })
        ));
    }

    #[test]
    fn modify_delete_recreate() {
        // M + D → tombstone, then A replaces tombstone with new inode.
        let tree = build(&[modify("/a", 1), delete("/a"), add("/a", 2)]);
        assert_eq!(tree.len(), 1);
        // A over Tombstone: the new inode replaces the tombstone.
        assert!(matches!(
            tree.get("/a"),
            Some(Dentry { target: Target::Inode(2), in_base: false, .. })
        ));
    }

    // ── delete_dir ────────────────────────────────────────────────────

    #[test]
    fn delete_dir_base_only_tombstone() {
        let tree = build(&[delete_dir("/d")]);
        assert_eq!(tree.len(), 1);
        assert!(matches!(tree.get("/d"), Some(Dentry { target: Target::None, .. })));
    }

    #[test]
    fn add_dir_then_delete_dir_cancels() {
        let tree = build(&[add_dir("/d", 1), delete_dir("/d")]);
        assert!(tree.is_empty(), "A + D should cancel: {:?}", tree);
    }

    #[test]
    fn delete_dir_with_children() {
        // Deleting a staged dir removes it and its children entirely.
        let tree = build(&[
            add_dir("/d", 1),
            add("/d/f1", 2),
            add("/d/f2", 3),
            delete_dir("/d"),
        ]);
        assert!(
            tree.is_empty(),
            "staged dir + children should cancel: {:?}",
            tree
        );
    }

    #[test]
    fn delete_dir_with_missing_dtype_uses_existing_dir_shape() {
        let tree = build(&[
            add_dir("/d", 1),
            add("/d/f", 2),
            Action::Delete {
                path: "/d".into(),
                dtype: None,
            },
        ]);
        assert!(
            tree.is_empty(),
            "existing staged dir should still cancel even when dtype is missing: {:?}",
            tree
        );
    }

    #[test]
    fn delete_base_dir_then_add_child() {
        // Delete a base directory, then add a file under it.
        // The tombstone should be a Dir node so walk_to_parent succeeds.
        let tree = build(&[delete_dir("/d"), add("/d/f1", 1)]);
        assert!(matches!(tree.get("/d"), Some(Dentry { target: Target::None, .. })));
        assert!(matches!(
            tree.get("/d/f1"),
            Some(Dentry { target: Target::Inode(1), in_base: false, .. })
        ));
    }

    // ── Delete intermediate directory ─────────────────────────────────

    #[test]
    fn delete_intermediate_dir_creates_tombstone() {
        // Add /a/b/c/file — creates intermediate /a, /a/b, /a/b/c.
        // Delete /a/b → should create Tombstone for the intermediate dir.
        let tree = build(&[add("/a/b/c/file", 1), delete_dir("/a/b")]);
        // /a/b was intermediate (Dir(passthrough,..)) → treated as base → negative dentry.
        assert!(
            matches!(tree.get("/a/b"), Some(Dentry { target: Target::None, .. })),
            "intermediate dir should get Tombstone: {:?}",
            tree
        );
    }

    // ── Checkpoint/Restore records ignored ────────────────────────────

    #[test]
    fn checkpoint_records_ignored_in_stream() {
        let tree = build(&[
            modify("/x", 1),
            Action::Modify {
                path: "/x".into(),
                dtype: Some(libc::DT_REG),
                ino: 2,
            },
        ]);
        assert_eq!(tree.len(), 1);
        assert!(matches!(
            tree.get("/x"),
            Some(Dentry { target: Target::Inode(2), in_base: true, .. })
        ));
    }

    #[test]
    fn restore_records_ignored_in_stream() {
        let tree = build(&[add("/a", 1), add("/b", 2)]);
        assert_eq!(tree.len(), 2);
        assert!(matches!(
            tree.get("/a"),
            Some(Dentry { target: Target::Inode(1), in_base: false, .. })
        ));
        assert!(matches!(
            tree.get("/b"),
            Some(Dentry { target: Target::Inode(2), in_base: false, .. })
        ));
    }

    // ── Self-rename / roundtrip / cycle ───────────────────────────────

    #[test]
    fn self_rename_is_noop() {
        let tree = build(&[rename("/a", "/a")]);
        assert!(tree.is_empty(), "R(a,a) should be a no-op: {:?}", tree);
    }

    #[test]
    fn self_replace_is_noop() {
        let tree = build(&[replace("/a", "/a")]);
        assert!(tree.is_empty(), "P(a,a) should be a no-op: {:?}", tree);
    }

    #[test]
    fn roundtrip_rename_produces_passthrough() {
        // a→tmp→a: file roundtrip removes the node from the tree.
        let tree = build(&[rename("/tmp", "/a"), rename("/a", "/tmp")]);
        assert_eq!(tree.len(), 0, "no staged changes");
        // File roundtrip — node removed entirely from tree.
        assert!(
            tree.nodes.get("a").is_none(),
            "expected file roundtrip to remove node, got {:?}",
            tree.nodes.get("a")
        );
    }

    #[test]
    fn roundtrip_rename_dir_produces_passthrough() {
        // Dir roundtrip (a→tmp→a) should produce a passthrough Dir.
        let tree = build(&[rename_dir("/tmp", "/a"), rename_dir("/a", "/tmp")]);
        assert_eq!(tree.len(), 0, "no staged changes");
        match tree.nodes.get("a") {
            Some(DirNode::Dir(d, _)) if d.is_passthrough() => {}
            other => panic!("expected passthrough Dir, got {:?}", other),
        }
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
            matches!(tree.get("/a"), Some(Dentry { target: Target::Path(Some(from)), .. }) if from == "/b"),
            "a should come from b: {:?}",
            tree
        );
        assert!(
            matches!(tree.get("/b"), Some(Dentry { target: Target::Path(Some(from)), .. }) if from == "/a"),
            "b should come from a: {:?}",
            tree
        );
    }

    #[test]
    fn three_step_roundtrip_rename_produces_passthrough() {
        // a→b→c→a: file roundtrip removes the node from the tree.
        let tree = build(&[rename("/b", "/a"), rename("/c", "/b"), rename("/a", "/c")]);
        assert_eq!(tree.len(), 0, "no staged changes after 3-step roundtrip");
        assert!(
            tree.nodes.get("a").is_none(),
            "expected file roundtrip to remove node, got {:?}",
            tree.nodes.get("a")
        );
    }

    // ── Empty tree ────────────────────────────────────────────────────

    #[test]
    fn empty_tree_dentries() {
        let tree = build(&[]);
        assert!(tree.is_empty());
    }

    // ── Symlink dtype ─────────────────────────────────────────────────

    #[test]
    fn add_symlink_dtype() {
        let tree = build(&[add_symlink("/link", 1)]);
        assert_eq!(tree.len(), 1);
        assert!(matches!(
            tree.get("/link"),
            Some(Dentry { target: Target::Inode(1), in_base: false })
        ));
    }

    #[test]
    fn rename_symlink_dtype() {
        let tree = build(&[rename_symlink("/new", "/old")]);
        assert_eq!(tree.len(), 2);
        assert!(matches!(tree.get("/old"), Some(Dentry { target: Target::None, .. })));
        assert!(matches!(
            tree.get("/new"),
            Some(Dentry { target: Target::Path(Some(_)),
                ..
            })
        ));
    }

    #[test]
    fn add_then_delete_symlink_cancels() {
        let tree = build(&[
            add_symlink("/link", 1),
            Action::Delete {
                path: "/link".into(),
                dtype: Some(libc::DT_LNK),
            },
        ]);
        assert!(
            tree.is_empty(),
            "A + D should cancel for symlinks: {:?}",
            tree
        );
    }

    // ── Replace with directory ────────────────────────────────────────

    #[test]
    fn replace_dir_base_only() {
        let tree = build(&[replace_dir("/dst", "/src")]);
        assert_eq!(tree.len(), 2);
        assert!(matches!(tree.get("/src"), Some(Dentry { target: Target::None, .. })));
        assert!(
            matches!(tree.get("/dst"), Some(Dentry { target: Target::Path(Some(from)), .. }) if from == "/src")
        );
    }

    #[test]
    fn replace_dir_with_children() {
        let tree = build(&[
            add_dir("/src", 1),
            add("/src/child", 2),
            replace_dir("/dst", "/src"),
        ]);
        assert!(tree.get("/dst").is_some(), "missing /dst");
        assert!(tree.get("/dst/child").is_some(), "missing /dst/child");
        assert!(tree.get("/src").is_none(), "stale /src");
        assert!(tree.get("/src/child").is_none(), "stale /src/child");
        // Destination should have in_base=true (from P tag)
        assert!(matches!(
            tree.get("/dst"),
            Some(Dentry { target: Target::Inode(_), in_base: true, .. })
        ));
    }

    // ── Base-only directory rename ────────────────────────────────────

    #[test]
    fn rename_base_only_dir() {
        let tree = build(&[rename_dir("/new", "/old")]);
        assert_eq!(tree.len(), 2);
        assert!(matches!(tree.get("/old"), Some(Dentry { target: Target::None, .. })));
        assert!(
            matches!(tree.get("/new"), Some(Dentry { target: Target::Path(Some(from)), .. }) if from == "/old")
        );
    }

    #[test]
    fn rename_dir_with_missing_dtype_moves_existing_subtree() {
        let tree = build(&[
            add_dir("/old", 1),
            add("/old/file", 2),
            Action::Rename {
                src: "/old".into(),
                dst: "/new".into(),
                dtype: None,
            },
        ]);
        assert!(tree.get("/old").is_none(), "old dir should be gone");
        assert!(tree.get("/old/file").is_none(), "old subtree should be gone");
        assert!(tree.get("/new").is_some(), "new dir should exist");
        assert!(tree.get("/new/file").is_some(), "subtree should move with dir");
    }

    // ── Replace chain ─────────────────────────────────────────────────

    #[test]
    fn replace_chain() {
        // P(/b, /a) then P(/c, /b): both a and b are base files.
        let tree = build(&[replace("/b", "/a"), replace("/c", "/b")]);
        // /a had base content → Tombstone at /a.
        // /b had Link (in_base=true from Replace) → moved to /c, tombstone at /b.
        // /c gets Link(src=/a, in_base=true).
        assert_eq!(tree.len(), 3, "expected 3 entries: {:?}", tree);
        assert!(
            matches!(tree.get("/a"), Some(Dentry { target: Target::None, .. })),
            "missing tombstone at /a: {:?}",
            tree
        );
        assert!(
            matches!(tree.get("/b"), Some(Dentry { target: Target::None, .. })),
            "missing tombstone at /b: {:?}",
            tree
        );
        assert!(
            matches!(tree.get("/c"), Some(Dentry { target: Target::Path(Some(from)), .. }) if from == "/a"),
            "/c should be Link from /a: {:?}",
            tree
        );
    }

    #[test]
    fn mixed_rename_replace_chain() {
        // R(/b, /a) then P(/c, /b): a is base, b is base (destination of P).
        let tree = build(&[rename("/b", "/a"), replace("/c", "/b")]);
        // R(/b, /a): Link at /b (in_base=false), Tombstone at /a.
        // P(/c, /b): move /b to /c, in_base=true. /b was in_base=false → no tombstone at /b.
        assert_eq!(tree.len(), 2, "expected 2 entries: {:?}", tree);
        assert!(
            matches!(tree.get("/a"), Some(Dentry { target: Target::None, .. })),
            "missing tombstone at /a: {:?}",
            tree
        );
        assert!(
            matches!(tree.get("/c"), Some(Dentry { target: Target::Path(Some(from)), .. }) if from == "/a"),
            "/c should be Link from /a: {:?}",
            tree
        );
    }

    // ── Dir rename + subsequent child operations ──────────────────────

    #[test]
    fn dir_rename_then_add_child() {
        let tree = build(&[
            add_dir("/old", 1),
            rename_dir("/new", "/old"),
            add("/new/child.txt", 2),
        ]);
        assert!(tree.get("/new").is_some(), "missing /new");
        assert!(
            tree.get("/new/child.txt").is_some(),
            "missing /new/child.txt"
        );
        assert!(
            matches!(
                tree.get("/new/child.txt"),
                Some(Dentry { target: Target::Inode(2), in_base: false, .. })
            ),
            "child should be Inode(ino=2): {:?}",
            tree
        );
    }

    #[test]
    fn dir_rename_then_delete_moved_child() {
        let tree = build(&[
            add_dir("/old", 1),
            add("/old/f1", 2),
            rename_dir("/new", "/old"),
            delete("/new/f1"),
        ]);
        // /old/f1 was in_base=false → delete cancels. /new has the dir, no /new/f1.
        assert!(tree.get("/new").is_some(), "missing /new");
        assert!(
            tree.get("/new/f1").is_none(),
            "/new/f1 should be cancelled: {:?}",
            tree
        );
    }

    // ── Rename over intermediate directory ────────────────────────────

    #[test]
    fn rename_into_intermediate_dir_position() {
        // A(/a/b/c/file) creates intermediates /a, /a/b, /a/b/c.
        // R(/other, /a/b) renames intermediate /a/b (which is a Dir(passthrough, ...) node).
        let tree = build(&[add("/a/b/c/file", 1), rename_dir("/other", "/a/b")]);
        // /a/b was an intermediate dir → source_had_base=true → negative dentry at /a/b.
        assert!(
            matches!(tree.get("/a/b"), Some(Dentry { target: Target::None, .. })),
            "/a/b should be negative dentry: {:?}",
            tree
        );
        // /other gets the subtree with /other/c/file.
        assert!(tree.get("/other/c/file").is_some(), "missing /other/c/file");
        assert!(
            matches!(
                tree.get("/other/c/file"),
                Some(Dentry { target: Target::Inode(1), .. })
            ),
            "/other/c/file should be Inode(ino=1): {:?}",
            tree
        );
        // /other should have a redirect dentry (from intermediate dir rename)
        assert!(
            matches!(tree.get("/other"), Some(Dentry { target: Target::Path(Some(from)), .. }) if from == "/a/b"),
            "/other should be redirect from /a/b: {:?}",
            tree
        );
    }

    #[test]
    fn replace_then_delete_tombstones_destination() {
        // Replace /a → /b (both base-only), then delete /b.
        // /b existed in base, so deleting the Link must leave a Tombstone
        // to hide the base content. Without it, base /b reappears.
        let cs = build(&[
            Action::Replace {
                src: "/a".into(),
                dst: "/b".into(),
                dtype: Some(libc::DT_REG),
            },
            Action::Delete {
                path: "/b".into(),
                dtype: Some(libc::DT_REG),
            },
        ]);
        // Both /a and /b should be tombstoned.
        assert!(
            matches!(cs.get("/a"), Some(Dentry { target: Target::None, .. })),
            "/a should be Tombstone: {:?}",
            cs
        );
        assert!(
            matches!(cs.get("/b"), Some(Dentry { target: Target::None, .. })),
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
        // Dentry: kind=1(StagedInode), in_base=false, ino=1
        expected.push(1); // kind
        expected.push(0); // in_base=false
        expected.extend_from_slice(&1u32.to_le_bytes()); // ino
        expected.extend_from_slice(&0u16.to_le_bytes()); // child_count = 0
        assert_eq!(buf, expected);
    }

    #[test]
    fn serialize_single_tombstone() {
        let tree = build(&[
            Action::Modify {
                path: "/old".into(),
                ino: 1,
                dtype: Some(libc::DT_REG),
            },
            Action::Delete {
                path: "/old".into(),
                dtype: Some(libc::DT_REG),
            },
        ]);
        let buf = tree.serialize();
        let mut expected = Vec::new();
        expected.extend_from_slice(&1u16.to_le_bytes()); // child_count = 1
        expected.extend_from_slice(&3u16.to_le_bytes()); // name_len = 3
        expected.extend_from_slice(b"old");
        // Dentry: kind=3(None/negative), in_base=true
        expected.push(3);
        expected.push(1); // in_base=true
        expected.extend_from_slice(&0u16.to_le_bytes()); // child_count = 0
        assert_eq!(buf, expected);
    }

    #[test]
    fn serialize_single_link() {
        let tree = build(&[Action::Rename {
            src: "/a.txt".into(),
            dst: "/b.txt".into(),
            dtype: Some(libc::DT_REG),
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
        assert_eq!(buf[cursor], 1); // in_base=true
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
        // Trailing: in_base + base_len + base_path
        assert_eq!(buf[cursor], 0); // in_base=false (Rename → dest not in base)
        cursor += 1;
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
        let tree = build(&[add_dir("/dir", 10), add("/dir/file", 20)]);
        let buf = tree.serialize();
        let mut cursor = 0usize;

        // Root child_count = 1
        let cc = u16::from_le_bytes([buf[cursor], buf[cursor + 1]]);
        cursor += 2;
        assert_eq!(cc, 1);

        // Node "dir": name_len=3, name="dir", dentry(kind=1,ino=10,in_base=false), child_count=1
        let name_len = u16::from_le_bytes([buf[cursor], buf[cursor + 1]]) as usize;
        cursor += 2;
        assert_eq!(&buf[cursor..cursor + name_len], b"dir");
        cursor += name_len;
        assert_eq!(buf[cursor], 1); // kind=STAGED_INODE
        cursor += 1;
        assert_eq!(buf[cursor], 0); // in_base=false
        cursor += 1;
        let ino = u32::from_le_bytes(buf[cursor..cursor + 4].try_into().unwrap());
        assert_eq!(ino, 10);
        cursor += 4;

        // child_count for "dir" subtree = 1
        let cc = u16::from_le_bytes([buf[cursor], buf[cursor + 1]]);
        cursor += 2;
        assert_eq!(cc, 1);

        // Node "file": name_len=4, name="file", dentry(kind=1,ino=20,in_base=false), child_count=0
        let name_len = u16::from_le_bytes([buf[cursor], buf[cursor + 1]]) as usize;
        cursor += 2;
        assert_eq!(&buf[cursor..cursor + name_len], b"file");
        cursor += name_len;
        assert_eq!(buf[cursor], 1); // kind=STAGED_INODE
        cursor += 1;
        assert_eq!(buf[cursor], 0); // in_base=false
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

        // Node "dir": kind=2 in_base=1 path_len=0 (passthrough scaffold)
        let name_len = u16::from_le_bytes([buf[cursor], buf[cursor + 1]]) as usize;
        cursor += 2;
        assert_eq!(&buf[cursor..cursor + name_len], b"dir");
        cursor += name_len;
        assert_eq!(buf[cursor], 2); // kind=REDIRECT (passthrough scaffold)
        cursor += 1;
        assert_eq!(buf[cursor], 1); // in_base=true
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
        cursor += 1; // in_base
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

        // Read names in order — each node: name_len(2) + name + kind(1) + ino(4) + in_base(1) + cc(2)
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
            cursor += 1 + 4 + 1; // kind + ino + in_base (StagedInode)
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
            DirNode::Dir(Dentry::passthrough(), DirTree::new()),
        );
        let buf = tree.serialize();
        // Should produce just child_count=0 (the empty passthrough dir is skipped)
        assert_eq!(buf, vec![0x00, 0x00]);
    }

    #[test]
    fn serialize_stale_intermediates_after_cancel() {
        // Add a deeply nested file then delete it.  The cancel removes the
        // leaf File node but leaves passthrough Dir intermediates.  The leaf-level
        // empty dir is filtered, but upper intermediates remain in the
        // serialized output.  The kernel tolerates these (skips nodes with
        // kind=0 and child_count=0).
        let tree = build(&[add("/a/b/c/file", 1), delete("/a/b/c/file")]);
        assert_eq!(tree.len(), 0, "no dentries after cancel");
        // Serialization still succeeds (doesn't panic).
        let _buf = tree.serialize();
    }

    #[test]
    fn serialize_partial_stale_intermediates() {
        // /a/b/c/file1 added + deleted (cancel), but /a/x still exists.
        // The stale /a/b branch remains in the tree but is harmless — the
        // kernel skips empty passthrough nodes.
        let tree = build(&[
            add("/a/b/c/file1", 1),
            add("/a/x", 2),
            delete("/a/b/c/file1"),
        ]);
        assert_eq!(tree.len(), 1, "only /a/x survives");
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
        // Verify the dentry layout for a StagedInode (Modify → in_base=true)
        let tree = build(&[Action::Modify {
            path: "/f".into(),
            ino: 42,
            dtype: Some(libc::DT_LNK),
        }]);
        let buf = tree.serialize();
        // Skip: root child_count(2) + name_len(2) + name(1) = offset 5
        assert_eq!(buf[5], 1); // kind=STAGED_INODE
        assert_eq!(buf[6], 1); // in_base=true (Modify)
        let ino = u32::from_le_bytes(buf[7..11].try_into().unwrap());
        assert_eq!(ino, 42);
    }

    #[test]
    fn serialize_link_bits_correct() {
        // Verify the dentry layout for a Redirect
        let tree = build(&[Action::Rename {
            src: "/src".into(),
            dst: "/dst".into(),
            dtype: Some(libc::DT_DIR),
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
        assert_eq!(buf[cursor], 0); // in_base=false (Rename: dest not in base)
        cursor += 1;

        // Trailing base_path
        let base_len = u16::from_le_bytes([buf[cursor], buf[cursor + 1]]) as usize;
        cursor += 2;
        assert_eq!(&buf[cursor..cursor + base_len], b"/src");
    }

    // serialize_unset_file_omitted test removed — File(Dentry::Unset) no longer
    // exists.  File roundtrips remove the node entirely from the tree.

    #[test]
    fn serialize_passthrough_dir_val_zero() {
        // A passthrough dir with children should serialize kind=0
        // but still emit the subtree.
        let mut tree = DirTree::new();
        let mut sub = DirTree::new();
        sub.nodes.insert(
            "child".to_string(),
            DirNode::File(Dentry {
                target: Target::Inode(1),
                in_base: false,
            }),
        );
        tree.nodes
            .insert("dir".to_string(), DirNode::Dir(Dentry::passthrough(), sub));
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
        // kind = 2, in_base=1, path_len=0 (passthrough scaffold)
        assert_eq!(buf[cursor], 2, "passthrough dir should have kind=2 (redirect scaffold)");
        cursor += 1;
        assert_eq!(buf[cursor], 1, "passthrough dir should have in_base=1");
        cursor += 1;
        assert_eq!(u16::from_le_bytes([buf[cursor], buf[cursor + 1]]), 0, "passthrough dir should have path_len=0");
        cursor += 2;
        // subtree child_count = 1 (the child)
        assert_eq!(u16::from_le_bytes([buf[cursor], buf[cursor + 1]]), 1);
    }

    #[test]
    fn serialize_negative_dentry_dir_dtype() {
        // delete_dir on a base-only dir produces a negative dentry.
        let tree = build(&[delete_dir("/d")]);
        let buf = tree.serialize();
        // Skip root child_count(2) + name_len(2) + name(1) = offset 5
        assert_eq!(buf[5], 3); // kind=None/negative
        assert_eq!(buf[6], 1); // in_base=true
    }

    #[test]
    fn serialize_negative_dentry_symlink_dtype() {
        // delete on a base-only symlink produces a negative dentry.
        let tree = build(&[Action::Delete {
            path: "/s".into(),
            dtype: Some(libc::DT_LNK),
        }]);
        let buf = tree.serialize();
        assert_eq!(buf[5], 3); // kind=None/negative
        assert_eq!(buf[6], 1); // in_base=true
    }

    #[test]
    fn serialize_after_roundtrip_rename_omits_passthrough() {
        // Roundtrip rename (a→tmp→a) removes the file node entirely,
        // so serialize() produces an empty tree.
        let tree = build(&[rename("/tmp", "/a"), rename("/a", "/tmp")]);
        assert_eq!(tree.len(), 0);
        let buf = tree.serialize();
        // Root child_count = 0 — the roundtrip file is removed from tree.
        assert_eq!(buf, vec![0x00, 0x00]);
    }

    #[test]
    fn roundtrip_rename_dir_preserves_subtree() {
        // Rename dir with children back to original — children should survive.
        // get() returns None for passthrough, so /a won't appear.
        let tree = build(&[
            add("/a/child", 1),
            rename_dir("/tmp", "/a"),
            rename_dir("/a", "/tmp"),
        ]);
        assert_eq!(tree.len(), 1, "only the child is a staged change");
        assert!(
            matches!(
                tree.get("/a/child"),
                Some(Dentry { target: Target::Inode(1), .. })
            ),
            "/a/child should survive roundtrip: {:?}",
            tree
        );
    }

    #[test]
    fn serialize_deeply_nested_passthrough_dirs() {
        // /a/b/c/file — three levels of passthrough scaffolds.
        // Each intermediate dir must serialize as tag=2, in_base=1, path_len=0
        // so the kernel's restore_inject_entry skips them correctly.
        let tree = build(&[add("/a/b/c/file", 42)]);
        let buf = tree.serialize();
        let mut cursor = 0usize;

        // Root child_count = 1
        assert_eq!(u16::from_le_bytes([buf[cursor], buf[cursor + 1]]), 1);
        cursor += 2;

        // For each passthrough dir (a, b, c): name + tag=2 + in_base=1 + path_len=0 + child_count=1
        for name in &["a", "b", "c"] {
            let nlen = u16::from_le_bytes([buf[cursor], buf[cursor + 1]]) as usize;
            cursor += 2;
            assert_eq!(&buf[cursor..cursor + nlen], name.as_bytes());
            cursor += nlen;
            assert_eq!(buf[cursor], 2, "{name}: tag should be 2 (PATH/passthrough)");
            cursor += 1;
            assert_eq!(buf[cursor], 1, "{name}: in_base should be 1");
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

        // Leaf file: name + tag=1 (INODE) + in_base=0 + ino=42 + child_count=0
        let nlen = u16::from_le_bytes([buf[cursor], buf[cursor + 1]]) as usize;
        cursor += 2;
        assert_eq!(&buf[cursor..cursor + nlen], b"file");
        cursor += nlen;
        assert_eq!(buf[cursor], 1); // tag=INODE
        cursor += 1;
        assert_eq!(buf[cursor], 0); // in_base=false
        cursor += 1;
        assert_eq!(u32::from_le_bytes(buf[cursor..cursor + 4].try_into().unwrap()), 42);
        cursor += 4;
        assert_eq!(u16::from_le_bytes([buf[cursor], buf[cursor + 1]]), 0);
        cursor += 2;

        assert_eq!(cursor, buf.len());
    }

    #[test]
    fn roundtrip_rename_with_sibling_changes() {
        // Roundtrip rename (a→tmp→a) should remove the roundtripped file
        // but leave other staged changes intact.
        let tree = build(&[
            add("/other", 1),
            rename("/tmp", "/a"),
            rename("/a", "/tmp"),
        ]);
        // "a" removed (roundtrip), "other" survives
        assert!(tree.nodes.get("a").is_none(), "roundtrip file should be removed");
        assert!(
            matches!(
                tree.get("/other"),
                Some(Dentry { target: Target::Inode(1), in_base: false, .. })
            ),
            "sibling should survive: {:?}",
            tree
        );

        let buf = tree.serialize();
        let mut cursor = 0usize;
        // Root child_count = 1 (only "other")
        assert_eq!(u16::from_le_bytes([buf[cursor], buf[cursor + 1]]), 1);
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
            DirNode::File(Dentry {
                target: Target::Path(Some("a".repeat(u16::MAX as usize + 1))),
                in_base: false,
            }),
        );
        tree.serialize();
    }
}

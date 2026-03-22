// agfs CLI — journal/tree.rs
//
// Dir tree builder. Applies journal records sequentially to build a tree
// representing the overlay state (the in-kernel dirent table).
//
// Three rules govern in_base and Tombstones:
//   1. in_base is set by the record tag: A/R → false, M/P → true.
//   2. Tombstone on vacate: when R/P moves a node away from a base path,
//      place a Tombstone at the vacated position.
//   3. Cancellation: D on a node with in_base=false → remove (cancel).
//      D on a node with in_base=true → Tombstone.

use std::collections::HashMap;

use super::types::*;

/// A dirent — the state of a single entry in the overlay.
#[derive(Debug, Clone, PartialEq)]
pub enum Dirent {
    Inode {
        ino: u64,
        dtype: DType,
        in_base: bool,
    },
    Link {
        base_path: String,
        dtype: DType,
        in_base: bool,
    },
    Tombstone,
}

impl Dirent {
    pub fn dtype(&self) -> DType {
        match self {
            Dirent::Inode { dtype, .. }
            | Dirent::Link { dtype, .. } => *dtype,
            Dirent::Tombstone => DType::File,
        }
    }

    pub fn in_base(&self) -> bool {
        match self {
            Dirent::Inode { in_base, .. } | Dirent::Link { in_base, .. } => *in_base,
            Dirent::Tombstone => true,
        }
    }

    /// Return the staged inode ID if this dirent carries one.
    pub fn ino(&self) -> Option<u64> {
        match self {
            Dirent::Inode { ino, .. } => Some(*ino),
            _ => None,
        }
    }

    /// True if this dirent involves the given path (as source or destination).
    pub fn matches_path(&self, dirent_path: &str, query: &str) -> bool {
        match self {
            Dirent::Inode { .. } | Dirent::Tombstone => dirent_path == query,
            Dirent::Link { base_path, .. } => dirent_path == query || base_path == query,
        }
    }

    fn set_in_base(&mut self, val: bool) {
        match self {
            Dirent::Inode { in_base, .. } | Dirent::Link { in_base, .. } => *in_base = val,
            Dirent::Tombstone => {}
        }
    }
}

/// A node in the dir tree.
#[derive(Debug, Clone, PartialEq)]
pub enum DirNode {
    File(Dirent),
    Dir(Option<Dirent>, DirTree),
}

impl DirNode {
    /// Wrap a dirent in the appropriate node type (File or Dir).
    fn leaf(dirent: Dirent) -> Self {
        if dirent.dtype() == DType::Dir {
            DirNode::Dir(Some(dirent), DirTree::new())
        } else {
            DirNode::File(dirent)
        }
    }
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
                let dtype = dtype.unwrap_or(DType::File);
                let dirent = Dirent::Inode {
                    ino,
                    dtype,
                    in_base: false,
                };
                self.set_dirent(path, dirent);
            }
            Action::Modify { path, dtype, ino } => {
                let dtype = dtype.unwrap_or(DType::File);
                let dirent = Dirent::Inode {
                    ino,
                    dtype,
                    in_base: true,
                };
                self.set_dirent(path, dirent);
            }
            Action::Delete { path, dtype } => {
                self.apply_delete(path, dtype);
            }
            Action::Rename { dst, src, dtype } => {
                let dtype = dtype.unwrap_or(DType::File);
                self.apply_rename(dst, src, dtype, false);
            }
            Action::Replace { dst, src, dtype } => {
                let dtype = dtype.unwrap_or(DType::File);
                self.apply_rename(dst, src, dtype, true);
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

    /// Number of dirents (files, dirs with metadata, tombstones) in the tree.
    pub fn len(&self) -> usize {
        self.nodes
            .values()
            .map(|n| match n {
                DirNode::File(_) => 1,
                DirNode::Dir(d, sub) => d.is_some() as usize + sub.len(),
            })
            .sum()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Visit each (full-path, dirent) pair by reference.
    pub fn for_each<F: FnMut(&str, &Dirent)>(&self, mut f: F) {
        self.visit_dirents(&mut f, &mut String::new());
    }

    /// Walk the tree and produce a flat list of (path, Dirent) pairs.
    pub fn into_dirents(&self) -> Vec<(String, Dirent)> {
        let mut entries = Vec::new();
        self.for_each(|path, dirent| entries.push((path.to_owned(), dirent.clone())));
        entries
    }

    // ── Internal helpers ──────────────────────────────────────────────

    /// Walk to a path (owned), creating intermediate Dir(None, ..) nodes as
    /// needed.  Extracts the leaf name from the path in-place via `drain`,
    /// avoiding allocation for the leaf component.
    fn walk_or_create_parent(&mut self, mut path: String) -> Option<(&mut DirTree, String)> {
        let last_slash = path.rfind('/')?;
        if last_slash + 1 >= path.len() {
            return None;
        }
        let mut current = self;
        for part in path[..last_slash].split('/').filter(|s| !s.is_empty()) {
            let node = current
                .nodes
                .entry(part.to_string())
                .or_insert_with(|| DirNode::Dir(None, DirTree::new()));
            match node {
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

    /// Set a dirent at the given path (owned).
    fn set_dirent(&mut self, path: String, dirent: Dirent) {
        let Some((parent, name)) = self.walk_or_create_parent(path) else {
            return;
        };
        if dirent.dtype() == DType::Dir
            && let Some(DirNode::Dir(existing_dirent, _)) = parent.nodes.get_mut(name.as_str()) {
                *existing_dirent = Some(dirent);
                return;
            }
        parent.nodes.insert(name, DirNode::leaf(dirent));
    }

    /// Apply a D record (owned path).
    fn apply_delete(&mut self, path: String, dtype: Option<DType>) {
        let Some((parent, name)) = self.walk_or_create_parent(path) else {
            return;
        };

        // Check what to do based on current state.
        let needs_tombstone = match parent.nodes.get(name.as_str()) {
            None | Some(DirNode::Dir(None, _)) => true,
            Some(DirNode::File(d)) | Some(DirNode::Dir(Some(d), _)) => d.in_base(),
        };

        if needs_tombstone {
            // Place or overwrite with tombstone (preserves Dir subtree).
            match parent.nodes.get_mut(name.as_str()) {
                Some(DirNode::File(d)) => *d = Dirent::Tombstone,
                Some(DirNode::Dir(d, _)) => *d = Some(Dirent::Tombstone),
                None => {
                    let node = if dtype.unwrap_or(DType::File) == DType::Dir {
                        DirNode::Dir(Some(Dirent::Tombstone), DirTree::new())
                    } else {
                        DirNode::File(Dirent::Tombstone)
                    };
                    parent.nodes.insert(name, node);
                }
            }
        } else {
            // in_base=false → cancel (remove)
            parent.nodes.remove(name.as_str());
        }
    }

    /// Apply R/P rename (owned paths).
    fn apply_rename(
        &mut self,
        dst_path: String,
        src_path: String,
        dtype: DType,
        dst_in_base: bool,
    ) {
        if dst_path == src_path {
            return;
        }

        // Detach source node
        let src_node = self.detach(&src_path);

        // Determine if source position had base content (for tombstone)
        let source_had_base = match &src_node {
            Some(DirNode::File(d)) => d.in_base(),
            Some(DirNode::Dir(Some(d), _)) => d.in_base(),
            Some(DirNode::Dir(None, _)) => true, // intermediate = base-only
            None => true,                        // no node = base-only
        };

        // Build the node to place at destination
        let mut dst_node = match src_node {
            Some(mut node) => {
                // Source existed — move it, update in_base
                match &mut node {
                    DirNode::File(d) => d.set_in_base(dst_in_base),
                    DirNode::Dir(Some(d), _) => d.set_in_base(dst_in_base),
                    DirNode::Dir(d @ None, _) => {
                        // Intermediate dir being explicitly renamed — create Link
                        *d = Some(Dirent::Link {
                            base_path: src_path.clone(),
                            dtype,
                            in_base: dst_in_base,
                        });
                    }
                }
                node
            }
            None => {
                // No source node — base-only file. Create Link.
                DirNode::leaf(Dirent::Link {
                    base_path: src_path.clone(),
                    dtype,
                    in_base: dst_in_base,
                })
            }
        };

        // Place tombstone at source if it had base content
        if source_had_base {
            let node = if dtype == DType::Dir {
                DirNode::Dir(Some(Dirent::Tombstone), DirTree::new())
            } else {
                DirNode::File(Dirent::Tombstone)
            };
            if let Some((parent, name)) = self.walk_or_create_parent(src_path) {
                match parent.nodes.get_mut(name.as_str()) {
                    Some(DirNode::File(d)) => *d = Dirent::Tombstone,
                    Some(DirNode::Dir(d, _)) => *d = Some(Dirent::Tombstone),
                    None => { parent.nodes.insert(name, node); }
                }
            }
        }

        // Roundtrip collapse: if dest ends up as a Link pointing to itself,
        // the rename chain was a no-op (e.g. a→b→a). Remove instead of inserting.
        let is_roundtrip = match &dst_node {
            DirNode::File(Dirent::Link { base_path, .. })
            | DirNode::Dir(Some(Dirent::Link { base_path, .. }), _) => base_path == &dst_path,
            _ => false,
        };

        // Place at destination (handle directory merging)
        let Some((parent, name)) = self.walk_or_create_parent(dst_path) else {
            return;
        };
        if is_roundtrip {
            parent.nodes.remove(name.as_str());
        } else {
            // If dest is a Dir and we're moving a Dir, merge children
            if let DirNode::Dir(_, src_subtree) = &mut dst_node
                && let Some(DirNode::Dir(_, existing_subtree)) = parent.nodes.remove(name.as_str())
                {
                    for (k, v) in existing_subtree.nodes {
                        src_subtree.nodes.entry(k).or_insert(v);
                    }
                }
            parent.nodes.insert(name, dst_node);
        }
    }

    /// Detach a node from the tree, returning it. Returns None if not found.
    fn detach(&mut self, path: &str) -> Option<DirNode> {
        let (parent, name) = self.walk_to_parent(path)?;
        parent.nodes.remove(name)
    }

    /// Walk the tree by reference, calling `f` for each (path, dirent).
    fn visit_dirents<F: FnMut(&str, &Dirent)>(&self, f: &mut F, prefix: &mut String) {
        for (name, node) in &self.nodes {
            let path_len = prefix.len();
            prefix.push('/');
            prefix.push_str(name);

            match node {
                DirNode::File(dirent) => f(prefix, dirent),
                DirNode::Dir(dirent, subtree) => {
                    if let Some(dirent) = dirent {
                        f(prefix, dirent);
                    }
                    subtree.visit_dirents(f, prefix);
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

    fn add(path: &str, ino: u64) -> Action {
        Action::Add {
            path: path.into(),
            dtype: Some(DType::File),
            ino,
        }
    }

    fn add_dir(path: &str, ino: u64) -> Action {
        Action::Add {
            path: path.into(),
            dtype: Some(DType::Dir),
            ino,
        }
    }

    fn modify(path: &str, ino: u64) -> Action {
        Action::Modify {
            path: path.into(),
            dtype: Some(DType::File),
            ino,
        }
    }

    fn delete(path: &str) -> Action {
        Action::Delete {
            path: path.into(),
            dtype: Some(DType::File),
        }
    }

    fn delete_dir(path: &str) -> Action {
        Action::Delete {
            path: path.into(),
            dtype: Some(DType::Dir),
        }
    }

    fn rename(dest: &str, src: &str) -> Action {
        Action::Rename {
            dst: dest.into(),
            src: src.into(),
            dtype: Some(DType::File),
        }
    }

    fn rename_dir(dest: &str, src: &str) -> Action {
        Action::Rename {
            dst: dest.into(),
            src: src.into(),
            dtype: Some(DType::Dir),
        }
    }

    fn replace(dest: &str, src: &str) -> Action {
        Action::Replace {
            dst: dest.into(),
            src: src.into(),
            dtype: Some(DType::File),
        }
    }

    fn replace_dir(dest: &str, src: &str) -> Action {
        Action::Replace {
            dst: dest.into(),
            src: src.into(),
            dtype: Some(DType::Dir),
        }
    }

    fn add_symlink(path: &str, ino: u64) -> Action {
        Action::Add {
            path: path.into(),
            dtype: Some(DType::Link),
            ino,
        }
    }

    fn rename_symlink(dest: &str, src: &str) -> Action {
        Action::Rename {
            dst: dest.into(),
            src: src.into(),
            dtype: Some(DType::Link),
        }
    }

    // ── Basic insert ──────────────────────────────────────────────────

    #[test]
    fn add_single_file() {
        let tree = build(&[add("/a", 1)]);
        let dirents = tree.into_dirents();
        assert_eq!(dirents.len(), 1);
        assert!(
            matches!(&dirents[0], (p, Dirent::Inode { ino: 1, in_base: false, .. }) if p == "/a")
        );
    }

    #[test]
    fn modify_single_file() {
        let tree = build(&[modify("/a", 1)]);
        let dirents = tree.into_dirents();
        assert_eq!(dirents.len(), 1);
        assert!(
            matches!(&dirents[0], (p, Dirent::Inode { ino: 1, in_base: true, .. }) if p == "/a")
        );
    }

    #[test]
    fn add_nested_file() {
        let tree = build(&[add("/dir/sub/file", 1)]);
        let dirents = tree.into_dirents();
        assert_eq!(dirents.len(), 1);
        assert!(
            matches!(&dirents[0], (p, Dirent::Inode { ino: 1, in_base: false, .. }) if p == "/dir/sub/file")
        );
    }

    // ── Cancellation ──────────────────────────────────────────────────

    #[test]
    fn add_then_delete_cancels() {
        let tree = build(&[add("/a", 1), delete("/a")]);
        let dirents = tree.into_dirents();
        assert!(dirents.is_empty(), "A + D should cancel: {:?}", dirents);
    }

    #[test]
    fn modify_then_delete_tombstone() {
        let tree = build(&[modify("/a", 1), delete("/a")]);
        let dirents = tree.into_dirents();
        assert_eq!(dirents.len(), 1);
        assert!(matches!(&dirents[0], (p, Dirent::Tombstone) if p == "/a"));
    }

    #[test]
    fn delete_base_only_tombstone() {
        let tree = build(&[delete("/a")]);
        let dirents = tree.into_dirents();
        assert_eq!(dirents.len(), 1);
        assert!(matches!(&dirents[0], (p, Dirent::Tombstone) if p == "/a"));
    }

    // ── Rename (R) ────────────────────────────────────────────────────

    #[test]
    fn rename_added_file() {
        let tree = build(&[add("/a", 1), rename("/b", "/a")]);
        let dirents = tree.into_dirents();
        // A + R: source was in_base=false → no tombstone at /a.
        // Destination gets the Inode with in_base=false (from R tag).
        assert_eq!(dirents.len(), 1);
        assert!(
            matches!(&dirents[0], (p, Dirent::Inode { ino: 1, in_base: false, .. }) if p == "/b")
        );
    }

    #[test]
    fn rename_base_only_file() {
        let tree = build(&[rename("/b", "/a")]);
        let dirents = tree.into_dirents();
        // Base-only: Link at /b, Tombstone at /a
        assert_eq!(dirents.len(), 2);
        let mut paths: Vec<_> = dirents.iter().map(|(p, _)| p.as_str()).collect();
        paths.sort();
        assert_eq!(paths, vec!["/a", "/b"]);
        assert!(
            dirents
                .iter()
                .any(|(p, c)| p == "/a" && matches!(c, Dirent::Tombstone))
        );
        assert!(
            dirents.iter().any(|(p, c)| p == "/b"
                && matches!(c, Dirent::Link { base_path: from, .. } if from == "/a"))
        );
    }

    // ── Replace (P) ──────────────────────────────────────────────────

    #[test]
    fn replace_base_only() {
        let tree = build(&[replace("/b", "/a")]);
        let dirents = tree.into_dirents();
        // P: dest in_base=true, source had base content → Tombstone at /a, Link at /b
        assert_eq!(dirents.len(), 2);
        assert!(
            dirents
                .iter()
                .any(|(p, c)| p == "/a" && matches!(c, Dirent::Tombstone))
        );
        assert!(
            dirents.iter().any(|(p, c)| p == "/b"
                && matches!(c, Dirent::Link { base_path: from, .. } if from == "/a"))
        );
    }

    // ── Rename chain ──────────────────────────────────────────────────

    #[test]
    fn rename_chain() {
        let tree = build(&[rename("/b", "/a"), rename("/c", "/b")]);
        let dirents = tree.into_dirents();
        // a→b→c: Tombstone at /a, nothing at /b (not in base), Link at /c
        assert_eq!(dirents.len(), 2);
        assert!(
            dirents
                .iter()
                .any(|(p, c)| p == "/a" && matches!(c, Dirent::Tombstone))
        );
        assert!(
            dirents.iter().any(|(p, c)| p == "/c"
                && matches!(c, Dirent::Link { base_path: from, .. } if from == "/a"))
        );
    }

    // ── Rename then delete ────────────────────────────────────────────

    #[test]
    fn rename_then_delete_base_file() {
        let tree = build(&[rename("/b", "/a"), delete("/b")]);
        let dirents = tree.into_dirents();
        // R(/b, /a): Link at /b (in_base=false), Tombstone at /a
        // D(/b): in_base=false → cancel
        // Result: just Tombstone at /a
        assert_eq!(dirents.len(), 1);
        assert!(matches!(&dirents[0], (p, Dirent::Tombstone) if p == "/a"));
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
        let dirents = tree.into_dirents();
        let paths: Vec<_> = dirents.iter().map(|(p, _)| p.as_str()).collect();
        assert!(paths.contains(&"/newdir"), "missing /newdir: {:?}", paths);
        assert!(
            paths.contains(&"/newdir/f1"),
            "missing /newdir/f1: {:?}",
            paths
        );
        assert!(
            paths.contains(&"/newdir/f2"),
            "missing /newdir/f2: {:?}",
            paths
        );
        assert!(!paths.contains(&"/dir"), "stale /dir: {:?}", paths);
        assert!(!paths.contains(&"/dir/f1"), "stale /dir/f1: {:?}", paths);
    }

    // ── Multiple modifies ─────────────────────────────────────────────

    #[test]
    fn multiple_modifies_last_wins() {
        let tree = build(&[modify("/a", 1), modify("/a", 2), modify("/a", 3)]);
        let dirents = tree.into_dirents();
        assert_eq!(dirents.len(), 1);
        assert!(
            matches!(&dirents[0], (p, Dirent::Inode { ino: 3, in_base: true, .. }) if p == "/a")
        );
    }

    // ── Rename + modify at dest ───────────────────────────────────────

    #[test]
    fn rename_then_modify_at_dest() {
        // R(/b, /a) then M(/b, ino=5): base file renamed, then modified at dest.
        // Tree: Link at /b replaced by Inode(ino=5, in_base=true), Tombstone at /a.
        let tree = build(&[rename("/b", "/a"), modify("/b", 5)]);
        let dirents = tree.into_dirents();
        assert_eq!(dirents.len(), 2);
        assert!(
            dirents
                .iter()
                .any(|(p, c)| p == "/a" && matches!(c, Dirent::Tombstone))
        );
        assert!(dirents.iter().any(|(p, c)| p == "/b"
            && matches!(
                c,
                Dirent::Inode {
                    ino: 5,
                    in_base: true,
                    ..
                }
            )));
    }

    #[test]
    fn replace_then_modify_at_dest() {
        // P(/b, /a) then M(/b, ino=5): overwrites base /b with renamed /a, then modified.
        let tree = build(&[replace("/b", "/a"), modify("/b", 5)]);
        let dirents = tree.into_dirents();
        assert_eq!(dirents.len(), 2);
        assert!(
            dirents
                .iter()
                .any(|(p, c)| p == "/a" && matches!(c, Dirent::Tombstone))
        );
        assert!(dirents.iter().any(|(p, c)| p == "/b"
            && matches!(
                c,
                Dirent::Inode {
                    ino: 5,
                    in_base: true,
                    ..
                }
            )));
    }

    // ── Rename over tombstone ─────────────────────────────────────────

    #[test]
    fn rename_over_tombstone() {
        // Delete /b (creates tombstone), then rename /a → /b.
        // The rename replaces the tombstone with a Link.
        let tree = build(&[delete("/b"), rename("/b", "/a")]);
        let dirents = tree.into_dirents();
        assert_eq!(dirents.len(), 2);
        assert!(
            dirents
                .iter()
                .any(|(p, c)| p == "/a" && matches!(c, Dirent::Tombstone))
        );
        assert!(
            dirents.iter().any(|(p, c)| p == "/b"
                && matches!(c, Dirent::Link { base_path: from, .. } if from == "/a"))
        );
    }

    // ── Add then rename (staged rename) ───────────────────────────────

    #[test]
    fn add_then_rename_preserves_inode() {
        // A(/a, ino=1) then R(/b, /a): staged file moved, inode preserved.
        let tree = build(&[add("/a", 1), rename("/b", "/a")]);
        let dirents = tree.into_dirents();
        // Source was in_base=false → no tombstone at /a.
        // Inode moved to /b with in_base=false.
        assert_eq!(dirents.len(), 1);
        assert!(
            matches!(&dirents[0], (p, Dirent::Inode { ino: 1, in_base: false, .. }) if p == "/b")
        );
    }

    // ── Create-delete-recreate ────────────────────────────────────────

    #[test]
    fn add_delete_add_same_path() {
        // A + D cancels, then A creates fresh.
        let tree = build(&[add("/a", 1), delete("/a"), add("/a", 2)]);
        let dirents = tree.into_dirents();
        assert_eq!(dirents.len(), 1);
        assert!(
            matches!(&dirents[0], (p, Dirent::Inode { ino: 2, in_base: false, .. }) if p == "/a")
        );
    }

    #[test]
    fn modify_delete_recreate() {
        // M + D → tombstone, then A replaces tombstone with new inode.
        let tree = build(&[modify("/a", 1), delete("/a"), add("/a", 2)]);
        let dirents = tree.into_dirents();
        assert_eq!(dirents.len(), 1);
        // A over Tombstone: the new inode replaces the tombstone.
        assert!(
            matches!(&dirents[0], (p, Dirent::Inode { ino: 2, in_base: false, .. }) if p == "/a")
        );
    }

    // ── delete_dir ────────────────────────────────────────────────────

    #[test]
    fn delete_dir_base_only_tombstone() {
        let tree = build(&[delete_dir("/d")]);
        let dirents = tree.into_dirents();
        assert_eq!(dirents.len(), 1);
        assert!(matches!(&dirents[0], (p, Dirent::Tombstone) if p == "/d"));
    }

    #[test]
    fn add_dir_then_delete_dir_cancels() {
        let tree = build(&[add_dir("/d", 1), delete_dir("/d")]);
        let dirents = tree.into_dirents();
        assert!(dirents.is_empty(), "A + D should cancel: {:?}", dirents);
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
        let dirents = tree.into_dirents();
        assert!(
            dirents.is_empty(),
            "staged dir + children should cancel: {:?}",
            dirents
        );
    }

    #[test]
    fn delete_base_dir_then_add_child() {
        // Delete a base directory, then add a file under it.
        // The tombstone should be a Dir node so walk_to_parent succeeds.
        let tree = build(&[delete_dir("/d"), add("/d/f1", 1)]);
        let dirents = tree.into_dirents();
        assert!(
            dirents
                .iter()
                .any(|(p, c)| p == "/d" && matches!(c, Dirent::Tombstone))
        );
        assert!(dirents.iter().any(|(p, c)| p == "/d/f1"
            && matches!(
                c,
                Dirent::Inode {
                    ino: 1,
                    in_base: false,
                    ..
                }
            )));
    }

    // ── Directory merge during rename ─────────────────────────────────

    #[test]
    fn dir_rename_merges_with_existing_children() {
        // /dest has an intermediate child from a prior add.
        // /src is renamed to /dest — src's subtree merges, existing children preserved.
        let tree = build(&[
            add("/dest/existing", 1),
            add_dir("/src", 2),
            add("/src/new", 3),
            rename_dir("/dest", "/src"),
        ]);
        let dirents = tree.into_dirents();
        let paths: Vec<_> = dirents.iter().map(|(p, _)| p.as_str()).collect();
        assert!(paths.contains(&"/dest"), "missing /dest: {:?}", paths);
        assert!(
            paths.contains(&"/dest/existing"),
            "lost existing child: {:?}",
            paths
        );
        assert!(
            paths.contains(&"/dest/new"),
            "lost moved child: {:?}",
            paths
        );
        assert!(!paths.contains(&"/src"), "stale /src: {:?}", paths);
    }

    #[test]
    fn dir_rename_merge_src_wins_on_conflict() {
        // Both /src and /dest have a child "f". Source child should win.
        let tree = build(&[
            add("/dest/f", 1),
            add_dir("/src", 2),
            add("/src/f", 3),
            rename_dir("/dest", "/src"),
        ]);
        let dirents = tree.into_dirents();
        // /dest/f should be from src (ino=3), not the old dest (ino=1)
        assert!(
            dirents.iter().any(|(p, c)| p == "/dest/f"
                && matches!(
                    c,
                    Dirent::Inode {
                        ino: 3,
                        in_base: false,
                        ..
                    }
                )),
            "src child should win on conflict: {:?}",
            dirents
        );
    }

    // ── Delete intermediate directory ─────────────────────────────────

    #[test]
    fn delete_intermediate_dir_creates_tombstone() {
        // Add /a/b/c/file — creates intermediate /a, /a/b, /a/b/c.
        // Delete /a/b → should create Tombstone for the intermediate dir.
        let tree = build(&[add("/a/b/c/file", 1), delete_dir("/a/b")]);
        let dirents = tree.into_dirents();
        // /a/b was intermediate (Dir(None,..)) → treated as base → Tombstone.
        assert!(
            dirents
                .iter()
                .any(|(p, c)| p == "/a/b" && matches!(c, Dirent::Tombstone)),
            "intermediate dir should get Tombstone: {:?}",
            dirents
        );
    }

    // ── Checkpoint/Restore records ignored ────────────────────────────

    #[test]
    fn checkpoint_records_ignored_in_stream() {
        let tree = build(&[
            modify("/x", 1),
            Action::Modify {
                path: "/x".into(),
                dtype: Some(DType::File),
                ino: 2,
            },
        ]);
        let dirents = tree.into_dirents();
        assert_eq!(dirents.len(), 1);
        assert!(
            matches!(&dirents[0], (p, Dirent::Inode { ino: 2, in_base: true, .. }) if p == "/x")
        );
    }

    #[test]
    fn restore_records_ignored_in_stream() {
        let tree = build(&[add("/a", 1), add("/b", 2)]);
        let dirents = tree.into_dirents();
        assert_eq!(dirents.len(), 2);
        assert!(dirents.iter().any(|(p, c)| p == "/a"
            && matches!(
                c,
                Dirent::Inode {
                    ino: 1,
                    in_base: false,
                    ..
                }
            )));
        assert!(dirents.iter().any(|(p, c)| p == "/b"
            && matches!(
                c,
                Dirent::Inode {
                    ino: 2,
                    in_base: false,
                    ..
                }
            )));
    }

    // ── Self-rename / roundtrip / cycle ───────────────────────────────

    #[test]
    fn self_rename_is_noop() {
        let tree = build(&[rename("/a", "/a")]);
        let dirents = tree.into_dirents();
        assert!(
            dirents.is_empty(),
            "R(a,a) should be a no-op: {:?}",
            dirents
        );
    }

    #[test]
    fn self_replace_is_noop() {
        let tree = build(&[replace("/a", "/a")]);
        let dirents = tree.into_dirents();
        assert!(
            dirents.is_empty(),
            "P(a,a) should be a no-op: {:?}",
            dirents
        );
    }

    #[test]
    fn roundtrip_rename_cancels() {
        // a→tmp→a should produce no net changes.
        let tree = build(&[rename("/tmp", "/a"), rename("/a", "/tmp")]);
        let dirents = tree.into_dirents();
        assert!(dirents.is_empty(), "a→tmp→a should cancel: {:?}", dirents);
    }

    #[test]
    fn three_cycle_swap() {
        // a→tmp, b→a, tmp→b: swaps a and b via tmp.
        let tree = build(&[
            rename("/tmp", "/a"),
            rename("/a", "/b"),
            rename("/b", "/tmp"),
        ]);
        let dirents = tree.into_dirents();
        assert!(
            !dirents.is_empty(),
            "swap should produce dirents: {:?}",
            dirents
        );
        // /a should be a Renamed from /b, /b should be a Renamed from /a
        assert!(
            dirents.iter().any(|(p, c)| p == "/a"
                && matches!(c, Dirent::Link { base_path: from, .. } if from == "/b")),
            "a should come from b: {:?}",
            dirents
        );
        assert!(
            dirents.iter().any(|(p, c)| p == "/b"
                && matches!(c, Dirent::Link { base_path: from, .. } if from == "/a")),
            "b should come from a: {:?}",
            dirents
        );
    }

    // ── Empty tree ────────────────────────────────────────────────────

    #[test]
    fn empty_tree_dirents() {
        let tree = build(&[]);
        let dirents = tree.into_dirents();
        assert!(dirents.is_empty());
    }

    // ── Symlink dtype ─────────────────────────────────────────────────

    #[test]
    fn add_symlink_dtype() {
        let tree = build(&[add_symlink("/link", 1)]);
        let dirents = tree.into_dirents();
        assert_eq!(dirents.len(), 1);
        assert!(
            matches!(&dirents[0], (p, Dirent::Inode { ino: 1, dtype: DType::Link, in_base: false }) if p == "/link")
        );
    }

    #[test]
    fn rename_symlink_dtype() {
        let tree = build(&[rename_symlink("/new", "/old")]);
        let dirents = tree.into_dirents();
        assert_eq!(dirents.len(), 2);
        assert!(
            dirents
                .iter()
                .any(|(p, c)| p == "/old" && matches!(c, Dirent::Tombstone))
        );
        assert!(dirents.iter().any(|(p, c)| p == "/new"
            && matches!(
                c,
                Dirent::Link {
                    dtype: DType::Link,
                    ..
                }
            )));
    }

    #[test]
    fn add_then_delete_symlink_cancels() {
        let tree = build(&[
            add_symlink("/link", 1),
            Action::Delete {
                path: "/link".into(),
                dtype: Some(DType::Link),
            },
        ]);
        let dirents = tree.into_dirents();
        assert!(
            dirents.is_empty(),
            "A + D should cancel for symlinks: {:?}",
            dirents
        );
    }

    // ── Replace with directory ────────────────────────────────────────

    #[test]
    fn replace_dir_base_only() {
        let tree = build(&[replace_dir("/dst", "/src")]);
        let dirents = tree.into_dirents();
        assert_eq!(dirents.len(), 2);
        assert!(
            dirents
                .iter()
                .any(|(p, c)| p == "/src" && matches!(c, Dirent::Tombstone))
        );
        assert!(dirents.iter().any(|(p, c)| p == "/dst" && matches!(c, Dirent::Link { base_path: from, dtype: DType::Dir, in_base: true } if from == "/src")));
    }

    #[test]
    fn replace_dir_with_children() {
        let tree = build(&[
            add_dir("/src", 1),
            add("/src/child", 2),
            replace_dir("/dst", "/src"),
        ]);
        let dirents = tree.into_dirents();
        let paths: Vec<_> = dirents.iter().map(|(p, _)| p.as_str()).collect();
        assert!(paths.contains(&"/dst"), "missing /dst: {:?}", paths);
        assert!(
            paths.contains(&"/dst/child"),
            "missing /dst/child: {:?}",
            paths
        );
        assert!(!paths.contains(&"/src"), "stale /src: {:?}", paths);
        assert!(
            !paths.contains(&"/src/child"),
            "stale /src/child: {:?}",
            paths
        );
        // Destination should have in_base=true (from P tag)
        assert!(
            dirents
                .iter()
                .any(|(p, c)| p == "/dst" && matches!(c, Dirent::Inode { in_base: true, .. }))
        );
    }

    // ── Base-only directory rename ────────────────────────────────────

    #[test]
    fn rename_base_only_dir() {
        let tree = build(&[rename_dir("/new", "/old")]);
        let dirents = tree.into_dirents();
        assert_eq!(dirents.len(), 2);
        assert!(
            dirents
                .iter()
                .any(|(p, c)| p == "/old" && matches!(c, Dirent::Tombstone))
        );
        assert!(dirents.iter().any(|(p, c)| p == "/new" && matches!(c, Dirent::Link { base_path: from, dtype: DType::Dir, in_base: false } if from == "/old")));
    }

    // ── Replace chain ─────────────────────────────────────────────────

    #[test]
    fn replace_chain() {
        // P(/b, /a) then P(/c, /b): both a and b are base files.
        let tree = build(&[replace("/b", "/a"), replace("/c", "/b")]);
        let dirents = tree.into_dirents();
        // /a had base content → Tombstone at /a.
        // /b had base content (P tag) → Tombstone at /b when moved away.
        // /c gets Link(base_path=/a, in_base=true).
        assert_eq!(dirents.len(), 3, "expected 3 entries: {:?}", dirents);
        assert!(
            dirents
                .iter()
                .any(|(p, c)| p == "/a" && matches!(c, Dirent::Tombstone)),
            "missing tombstone at /a: {:?}",
            dirents
        );
        assert!(
            dirents
                .iter()
                .any(|(p, c)| p == "/b" && matches!(c, Dirent::Tombstone)),
            "missing tombstone at /b: {:?}",
            dirents
        );
        assert!(dirents.iter().any(|(p, c)| p == "/c" && matches!(c, Dirent::Link { base_path: from, in_base: true, .. } if from == "/a")),
            "/c should be Link from /a with in_base=true: {:?}", dirents);
    }

    #[test]
    fn mixed_rename_replace_chain() {
        // R(/b, /a) then P(/c, /b): a is base, b is base (destination of P).
        let tree = build(&[rename("/b", "/a"), replace("/c", "/b")]);
        let dirents = tree.into_dirents();
        // R(/b, /a): Link at /b (in_base=false), Tombstone at /a.
        // P(/c, /b): move /b to /c, in_base=true. /b was in_base=false → no tombstone at /b.
        assert_eq!(dirents.len(), 2, "expected 2 entries: {:?}", dirents);
        assert!(
            dirents
                .iter()
                .any(|(p, c)| p == "/a" && matches!(c, Dirent::Tombstone)),
            "missing tombstone at /a: {:?}",
            dirents
        );
        assert!(dirents.iter().any(|(p, c)| p == "/c" && matches!(c, Dirent::Link { base_path: from, in_base: true, .. } if from == "/a")),
            "/c should be Link from /a with in_base=true: {:?}", dirents);
    }

    // ── Dir rename + subsequent child operations ──────────────────────

    #[test]
    fn dir_rename_then_add_child() {
        let tree = build(&[
            add_dir("/old", 1),
            rename_dir("/new", "/old"),
            add("/new/child.txt", 2),
        ]);
        let dirents = tree.into_dirents();
        let paths: Vec<_> = dirents.iter().map(|(p, _)| p.as_str()).collect();
        assert!(paths.contains(&"/new"), "missing /new: {:?}", paths);
        assert!(
            paths.contains(&"/new/child.txt"),
            "missing /new/child.txt: {:?}",
            paths
        );
        assert!(
            dirents.iter().any(|(p, c)| p == "/new/child.txt"
                && matches!(
                    c,
                    Dirent::Inode {
                        ino: 2,
                        in_base: false,
                        ..
                    }
                )),
            "child should be Inode(ino=2): {:?}",
            dirents
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
        let dirents = tree.into_dirents();
        // /old/f1 was in_base=false → delete cancels. /new has the dir, no /new/f1.
        let paths: Vec<_> = dirents.iter().map(|(p, _)| p.as_str()).collect();
        assert!(paths.contains(&"/new"), "missing /new: {:?}", paths);
        assert!(
            !paths.contains(&"/new/f1"),
            "/new/f1 should be cancelled: {:?}",
            dirents
        );
    }

    // ── Rename over intermediate directory ────────────────────────────

    #[test]
    fn rename_into_intermediate_dir_position() {
        // A(/a/b/c/file) creates intermediates /a, /a/b, /a/b/c.
        // R(/other, /a/b) renames intermediate /a/b (which is a Dir(None, ..) node).
        let tree = build(&[add("/a/b/c/file", 1), rename_dir("/other", "/a/b")]);
        let dirents = tree.into_dirents();
        let paths: Vec<_> = dirents.iter().map(|(p, _)| p.as_str()).collect();
        // /a/b was an intermediate dir → source_had_base=true → Tombstone at /a/b.
        assert!(
            dirents
                .iter()
                .any(|(p, c)| p == "/a/b" && matches!(c, Dirent::Tombstone)),
            "/a/b should be Tombstone: {:?}",
            dirents
        );
        // /other gets the subtree with /other/c/file.
        assert!(
            paths.contains(&"/other/c/file"),
            "missing /other/c/file: {:?}",
            paths
        );
        assert!(
            dirents
                .iter()
                .any(|(p, c)| p == "/other/c/file" && matches!(c, Dirent::Inode { ino: 1, .. })),
            "/other/c/file should be Inode(ino=1): {:?}",
            dirents
        );
        // /other should have a Link dirent (from intermediate dir rename)
        assert!(
            dirents.iter().any(|(p, c)| p == "/other"
                && matches!(c, Dirent::Link { base_path: from, .. } if from == "/a/b")),
            "/other should be Link from /a/b: {:?}",
            dirents
        );
    }
}

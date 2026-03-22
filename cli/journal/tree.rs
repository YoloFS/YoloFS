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
///
/// `in_base` indicates whether this path position had content in the base
/// filesystem before staging.  It determines cleanup behavior: when removed
/// or moved away, `in_base=true` leaves a Tombstone to hide the base content;
/// `in_base=false` cancels (just removes — nothing in base to hide).
#[derive(Debug, Clone, PartialEq)]
pub enum Dstate {
    StagedInode {
        ino: u32,
        dtype: u8,
        in_base: bool,
    },
    BasePath {
        /// The source path in the base filesystem (where the content lives).
        src: String,
        dtype: u8,
        in_base: bool,
    },
    Tombstone {
        dtype: u8,
    },
    Passthrough,
}

impl Dstate {
    pub fn dtype(&self) -> u8 {
        match self {
            Dstate::StagedInode { dtype, .. }
            | Dstate::BasePath { dtype, .. }
            | Dstate::Tombstone { dtype } => *dtype,
            Dstate::Passthrough => libc::DT_REG,
        }
    }

    pub fn in_base(&self) -> bool {
        match self {
            Dstate::StagedInode { in_base, .. } | Dstate::BasePath { in_base, .. } => *in_base,
            Dstate::Tombstone { .. } | Dstate::Passthrough => true,
        }
    }

    /// Return the staged inode ID if this dirent carries one.
    pub fn ino(&self) -> Option<u32> {
        match self {
            Dstate::StagedInode { ino, .. } => Some(*ino),
            _ => None,
        }
    }

    /// True if this dirent involves the given path (as source or destination).
    pub fn matches_path(&self, dirent_path: &str, query: &str) -> bool {
        match self {
            Dstate::StagedInode { .. } | Dstate::Tombstone { .. } | Dstate::Passthrough => {
                dirent_path == query
            }
            Dstate::BasePath { src, .. } => dirent_path == query || src == query,
        }
    }

    fn set_in_base(&mut self, val: bool) {
        match self {
            Dstate::StagedInode { in_base, .. } | Dstate::BasePath { in_base, .. } => *in_base = val,
            Dstate::Tombstone { .. } | Dstate::Passthrough => {}
        }
    }
}

/// A node in the dir tree.
#[derive(Debug, Clone, PartialEq)]
pub enum DirNode {
    File(Dstate),
    Dir(Dstate, DirTree),
}

impl DirNode {
    /// Wrap a dirent in the appropriate node type (File or Dir).
    fn leaf(dirent: Dstate) -> Self {
        if dirent.dtype() == libc::DT_DIR {
            DirNode::Dir(dirent, DirTree::new())
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
                let dtype = dtype.unwrap_or(libc::DT_REG);
                let dirent = Dstate::StagedInode {
                    ino,
                    dtype,
                    in_base: false,
                };
                self.set_dirent(path, dirent);
            }
            Action::Modify { path, dtype, ino } => {
                let dtype = dtype.unwrap_or(libc::DT_REG);
                let dirent = Dstate::StagedInode {
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
                let dtype = dtype.unwrap_or(libc::DT_REG);
                self.apply_rename(dst, src, dtype, false);
            }
            Action::Replace { dst, src, dtype } => {
                let dtype = dtype.unwrap_or(libc::DT_REG);
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
    /// Passthrough entries are excluded — they represent no staged change.
    pub fn len(&self) -> usize {
        self.nodes
            .values()
            .map(|n| match n {
                DirNode::File(Dstate::Passthrough) => 0,
                DirNode::File(_) => 1,
                DirNode::Dir(Dstate::Passthrough, sub) => sub.len(),
                DirNode::Dir(_, sub) => 1 + sub.len(),
            })
            .sum()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Visit each (full-path, dirent) pair by reference.
    pub fn for_each<F: FnMut(&str, &Dstate)>(&self, mut f: F) {
        self.visit_dirents(&mut f, &mut String::new());
    }

    /// Walk the tree and produce a flat list of (path, Dstate) pairs.
    pub fn into_dirents(&self) -> Vec<(String, Dstate)> {
        let mut entries = Vec::new();
        self.for_each(|path, dirent| entries.push((path.to_owned(), dirent.clone())));
        entries
    }

    /// Serialize the tree into a contiguous byte buffer for the restore ioctl.
    ///
    /// Wire format (all integers little-endian):
    ///   TreeBuf      := NodeList
    ///   NodeList     := child_count:le16  Node[child_count]
    ///   Node         := name_len:le16  name:u8[name_len]
    ///                   has_dirent:u8  [PackedDstate if has_dirent]
    ///                   child_count:le16  Node[child_count]
    ///   PackedDstate := packed:le64
    ///                   [base_len:le16  base_path:u8[base_len]  NUL:u8  if link]
    pub fn serialize(&self) -> Vec<u8> {
        let mut buf = Vec::new();
        self.serialize_into(&mut buf);
        buf
    }

    fn serialize_into(&self, buf: &mut Vec<u8>) {
        // Collect children sorted by name, skipping empty passthrough dirs
        // and passthrough file nodes.
        let mut children: Vec<(&str, &DirNode)> = self
            .nodes
            .iter()
            .filter(|(_, node)| match node {
                DirNode::Dir(Dstate::Passthrough, sub) if sub.nodes.is_empty() => false,
                DirNode::File(Dstate::Passthrough) => false,
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
                DirNode::File(dirent) => {
                    buf.push(1); // has_dirent
                    Self::serialize_dirent(dirent, buf);
                    buf.extend_from_slice(&0u16.to_le_bytes()); // child_count = 0
                }
                DirNode::Dir(dirent, subtree) => {
                    if matches!(dirent, Dstate::Passthrough) {
                        buf.push(0); // no dirent
                    } else {
                        buf.push(1); // has_dirent
                        Self::serialize_dirent(dirent, buf);
                    }
                    subtree.serialize_into(buf);
                }
            }
        }
    }

    fn serialize_dirent(dirent: &Dstate, buf: &mut Vec<u8>) {
        match dirent {
            Dstate::Tombstone { dtype } => {
                // Tombstone: (s64)val > 0, ino=0, in_base=1
                let packed: u64 = (dtype_pack(*dtype) << 60) | (1u64 << 59);
                buf.extend_from_slice(&packed.to_le_bytes());
            }
            Dstate::StagedInode {
                ino,
                dtype,
                in_base,
            } => {
                assert!(*ino > 0, "inode ino must be non-zero");
                let packed: u64 = (dtype_pack(*dtype) << 60)
                    | ((*in_base as u64) << 59)
                    | ((*ino as u64) << 16);
                // gen bits [15:0] zeroed — kernel assigns new_gen
                buf.extend_from_slice(&packed.to_le_bytes());
            }
            Dstate::BasePath {
                src,
                dtype,
                in_base,
            } => {
                let packed: u64 = (1u64 << 63)
                    | (dtype_pack(*dtype) << 60)
                    | ((*in_base as u64) << 59);
                // pointer bits [59:0] zeroed — base path travels inline
                buf.extend_from_slice(&packed.to_le_bytes());
                let bp = src.as_bytes();
                let bp_len: u16 = bp.len().try_into().expect("base_path too long");
                buf.extend_from_slice(&bp_len.to_le_bytes());
                buf.extend_from_slice(bp);
                buf.push(0); // NUL terminator
            }
            Dstate::Passthrough => {
                buf.extend_from_slice(&0u64.to_le_bytes());
            }
        }
    }

    // ── Internal helpers ──────────────────────────────────────────────

    /// Walk to a path (owned), creating intermediate Dir(Passthrough, ...) nodes as
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
                .or_insert_with(|| DirNode::Dir(Dstate::Passthrough, DirTree::new()));
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
    fn set_dirent(&mut self, path: String, dirent: Dstate) {
        let Some((parent, name)) = self.walk_or_create_parent(path) else {
            return;
        };
        if dirent.dtype() == libc::DT_DIR
            && let Some(DirNode::Dir(existing_dirent, _)) = parent.nodes.get_mut(name.as_str()) {
                *existing_dirent = dirent;
                return;
            }
        parent.nodes.insert(name, DirNode::leaf(dirent));
    }

    /// Apply a D record (owned path).
    fn apply_delete(&mut self, path: String, dtype: Option<u8>) {
        let Some((parent, name)) = self.walk_or_create_parent(path) else {
            return;
        };

        // Check what to do based on current state.
        let needs_tombstone = match parent.nodes.get(name.as_str()) {
            None | Some(DirNode::Dir(Dstate::Passthrough, _)) => true,
            Some(DirNode::File(d)) | Some(DirNode::Dir(d, _)) => d.in_base(),
        };

        if needs_tombstone {
            Self::place_tombstone_at(parent, name, dtype.unwrap_or(libc::DT_REG));
        } else {
            // in_base=false → cancel (remove)
            parent.nodes.remove(name.as_str());
        }
    }

    /// Place a Tombstone at a path, preserving any existing Dir subtree.
    fn place_tombstone(&mut self, path: String, dtype: u8) {
        if let Some((parent, name)) = self.walk_or_create_parent(path) {
            Self::place_tombstone_at(parent, name, dtype);
        }
    }

    /// Place a Tombstone in a parent's node map, preserving any existing Dir subtree.
    fn place_tombstone_at(parent: &mut DirTree, name: String, dtype: u8) {
        match parent.nodes.get_mut(name.as_str()) {
            Some(DirNode::File(d)) => *d = Dstate::Tombstone { dtype },
            Some(DirNode::Dir(d, _)) => *d = Dstate::Tombstone { dtype },
            None => {
                let node = if dtype == libc::DT_DIR {
                    DirNode::Dir(Dstate::Tombstone { dtype }, DirTree::new())
                } else {
                    DirNode::File(Dstate::Tombstone { dtype })
                };
                parent.nodes.insert(name, node);
            }
        }
    }

    /// Apply R/P rename (owned paths).
    fn apply_rename(
        &mut self,
        dst_path: String,
        src_path: String,
        dtype: u8,
        dst_in_base: bool,
    ) {
        if dst_path == src_path {
            return;
        }

        // Detach source node
        let src_node = self.detach(&src_path);

        // Determine if source position had base content (for tombstone)
        let source_had_base = match &src_node {
            Some(DirNode::File(d)) | Some(DirNode::Dir(d, _)) => d.in_base(),
            None => true, // no node = base-only
        };

        // Build the node to place at destination
        let mut dst_node = match src_node {
            Some(mut node) => {
                // Source existed — move it, update in_base
                match &mut node {
                    DirNode::File(d) => d.set_in_base(dst_in_base),
                    DirNode::Dir(d, _) if matches!(d, Dstate::Passthrough) => {
                        // Intermediate dir being explicitly renamed — create BasePath
                        *d = Dstate::BasePath {
                            src: src_path.clone(),
                            dtype,
                            in_base: dst_in_base,
                        };
                    }
                    DirNode::Dir(d, _) => d.set_in_base(dst_in_base),
                }
                node
            }
            None => {
                // No source node — base-only file. Create BasePath.
                DirNode::leaf(Dstate::BasePath {
                    src: src_path.clone(),
                    dtype,
                    in_base: dst_in_base,
                })
            }
        };

        // Place tombstone at source if it had base content
        if source_had_base {
            self.place_tombstone(src_path, dtype);
        }

        // Roundtrip collapse: if dest ends up as a BasePath pointing to itself,
        // the rename chain was a no-op (e.g. a→b→a). Replace with Passthrough.
        let is_roundtrip = match &dst_node {
            DirNode::File(Dstate::BasePath { src, .. })
            | DirNode::Dir(Dstate::BasePath { src, .. }, _) => src == &dst_path,
            _ => false,
        };

        // Place at destination (handle directory merging)
        let Some((parent, name)) = self.walk_or_create_parent(dst_path) else {
            return;
        };
        if is_roundtrip {
            // Replace with Passthrough — the rename chain was a no-op.
            match dst_node {
                DirNode::Dir(_, subtree) => {
                    parent.nodes.insert(name, DirNode::Dir(Dstate::Passthrough, subtree));
                }
                DirNode::File(_) => {
                    parent.nodes.insert(name, DirNode::File(Dstate::Passthrough));
                }
            }
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
    fn visit_dirents<F: FnMut(&str, &Dstate)>(&self, f: &mut F, prefix: &mut String) {
        for (name, node) in &self.nodes {
            let path_len = prefix.len();
            prefix.push('/');
            prefix.push_str(name);

            match node {
                DirNode::File(dirent) => f(prefix, dirent),
                DirNode::Dir(dirent, subtree) => {
                    // Skip Passthrough dir entries (intermediate dirs).
                    if !matches!(dirent, Dstate::Passthrough) {
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
        let dirents = tree.into_dirents();
        assert_eq!(dirents.len(), 1);
        assert!(
            matches!(&dirents[0], (p, Dstate::StagedInode { ino: 1, in_base: false, .. }) if p == "/a")
        );
    }

    #[test]
    fn modify_single_file() {
        let tree = build(&[modify("/a", 1)]);
        let dirents = tree.into_dirents();
        assert_eq!(dirents.len(), 1);
        assert!(
            matches!(&dirents[0], (p, Dstate::StagedInode { ino: 1, in_base: true, .. }) if p == "/a")
        );
    }

    #[test]
    fn add_nested_file() {
        let tree = build(&[add("/dir/sub/file", 1)]);
        let dirents = tree.into_dirents();
        assert_eq!(dirents.len(), 1);
        assert!(
            matches!(&dirents[0], (p, Dstate::StagedInode { ino: 1, in_base: false, .. }) if p == "/dir/sub/file")
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
        assert!(matches!(&dirents[0], (p, Dstate::Tombstone { .. }) if p == "/a"));
    }

    #[test]
    fn delete_base_only_tombstone() {
        let tree = build(&[delete("/a")]);
        let dirents = tree.into_dirents();
        assert_eq!(dirents.len(), 1);
        assert!(matches!(&dirents[0], (p, Dstate::Tombstone { .. }) if p == "/a"));
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
            matches!(&dirents[0], (p, Dstate::StagedInode { ino: 1, in_base: false, .. }) if p == "/b")
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
                .any(|(p, c)| p == "/a" && matches!(c, Dstate::Tombstone { .. }))
        );
        assert!(
            dirents.iter().any(|(p, c)| p == "/b"
                && matches!(c, Dstate::BasePath { src: from, .. } if from == "/a"))
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
                .any(|(p, c)| p == "/a" && matches!(c, Dstate::Tombstone { .. }))
        );
        assert!(
            dirents.iter().any(|(p, c)| p == "/b"
                && matches!(c, Dstate::BasePath { src: from, .. } if from == "/a"))
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
                .any(|(p, c)| p == "/a" && matches!(c, Dstate::Tombstone { .. }))
        );
        assert!(
            dirents.iter().any(|(p, c)| p == "/c"
                && matches!(c, Dstate::BasePath { src: from, .. } if from == "/a"))
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
        assert!(matches!(&dirents[0], (p, Dstate::Tombstone { .. }) if p == "/a"));
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
            matches!(&dirents[0], (p, Dstate::StagedInode { ino: 3, in_base: true, .. }) if p == "/a")
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
                .any(|(p, c)| p == "/a" && matches!(c, Dstate::Tombstone { .. }))
        );
        assert!(dirents.iter().any(|(p, c)| p == "/b"
            && matches!(
                c,
                Dstate::StagedInode {
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
                .any(|(p, c)| p == "/a" && matches!(c, Dstate::Tombstone { .. }))
        );
        assert!(dirents.iter().any(|(p, c)| p == "/b"
            && matches!(
                c,
                Dstate::StagedInode {
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
                .any(|(p, c)| p == "/a" && matches!(c, Dstate::Tombstone { .. }))
        );
        assert!(
            dirents.iter().any(|(p, c)| p == "/b"
                && matches!(c, Dstate::BasePath { src: from, .. } if from == "/a"))
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
            matches!(&dirents[0], (p, Dstate::StagedInode { ino: 1, in_base: false, .. }) if p == "/b")
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
            matches!(&dirents[0], (p, Dstate::StagedInode { ino: 2, in_base: false, .. }) if p == "/a")
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
            matches!(&dirents[0], (p, Dstate::StagedInode { ino: 2, in_base: false, .. }) if p == "/a")
        );
    }

    // ── delete_dir ────────────────────────────────────────────────────

    #[test]
    fn delete_dir_base_only_tombstone() {
        let tree = build(&[delete_dir("/d")]);
        let dirents = tree.into_dirents();
        assert_eq!(dirents.len(), 1);
        assert!(matches!(&dirents[0], (p, Dstate::Tombstone { .. }) if p == "/d"));
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
                .any(|(p, c)| p == "/d" && matches!(c, Dstate::Tombstone { .. }))
        );
        assert!(dirents.iter().any(|(p, c)| p == "/d/f1"
            && matches!(
                c,
                Dstate::StagedInode {
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
                    Dstate::StagedInode {
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
                .any(|(p, c)| p == "/a/b" && matches!(c, Dstate::Tombstone { .. })),
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
                dtype: Some(libc::DT_REG),
                ino: 2,
            },
        ]);
        let dirents = tree.into_dirents();
        assert_eq!(dirents.len(), 1);
        assert!(
            matches!(&dirents[0], (p, Dstate::StagedInode { ino: 2, in_base: true, .. }) if p == "/x")
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
                Dstate::StagedInode {
                    ino: 1,
                    in_base: false,
                    ..
                }
            )));
        assert!(dirents.iter().any(|(p, c)| p == "/b"
            && matches!(
                c,
                Dstate::StagedInode {
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
    fn roundtrip_rename_produces_passthrough() {
        // a→tmp→a should produce Passthrough at /a (no net staged change).
        let tree = build(&[rename("/tmp", "/a"), rename("/a", "/tmp")]);
        assert_eq!(tree.len(), 0, "no staged changes");
        let dirents = tree.into_dirents();
        assert_eq!(dirents.len(), 1, "one Passthrough entry: {:?}", dirents);
        assert!(
            matches!(&dirents[0], (p, Dstate::Passthrough) if p == "/a"),
            "expected Passthrough at /a: {:?}",
            dirents
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
        let dirents = tree.into_dirents();
        assert!(
            !dirents.is_empty(),
            "swap should produce dirents: {:?}",
            dirents
        );
        // /a should be a Renamed from /b, /b should be a Renamed from /a
        assert!(
            dirents.iter().any(|(p, c)| p == "/a"
                && matches!(c, Dstate::BasePath { src: from, .. } if from == "/b")),
            "a should come from b: {:?}",
            dirents
        );
        assert!(
            dirents.iter().any(|(p, c)| p == "/b"
                && matches!(c, Dstate::BasePath { src: from, .. } if from == "/a")),
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
            matches!(&dirents[0], (p, Dstate::StagedInode { ino: 1, dtype: libc::DT_LNK, in_base: false }) if p == "/link")
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
                .any(|(p, c)| p == "/old" && matches!(c, Dstate::Tombstone { .. }))
        );
        assert!(dirents.iter().any(|(p, c)| p == "/new"
            && matches!(
                c,
                Dstate::BasePath {
                    dtype: libc::DT_LNK,
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
                dtype: Some(libc::DT_LNK),
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
                .any(|(p, c)| p == "/src" && matches!(c, Dstate::Tombstone { .. }))
        );
        assert!(dirents.iter().any(|(p, c)| p == "/dst" && matches!(c, Dstate::BasePath { src: from, dtype: libc::DT_DIR, .. } if from == "/src")));
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
                .any(|(p, c)| p == "/dst" && matches!(c, Dstate::StagedInode { in_base: true, .. }))
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
                .any(|(p, c)| p == "/old" && matches!(c, Dstate::Tombstone { .. }))
        );
        assert!(dirents.iter().any(|(p, c)| p == "/new" && matches!(c, Dstate::BasePath { src: from, dtype: libc::DT_DIR, .. } if from == "/old")));
    }

    // ── Replace chain ─────────────────────────────────────────────────

    #[test]
    fn replace_chain() {
        // P(/b, /a) then P(/c, /b): both a and b are base files.
        let tree = build(&[replace("/b", "/a"), replace("/c", "/b")]);
        let dirents = tree.into_dirents();
        // /a had base content → Tombstone at /a.
        // /b had Link (in_base=true from Replace) → moved to /c, tombstone at /b.
        // /c gets Link(src=/a, in_base=true).
        assert_eq!(dirents.len(), 3, "expected 3 entries: {:?}", dirents);
        assert!(
            dirents
                .iter()
                .any(|(p, c)| p == "/a" && matches!(c, Dstate::Tombstone { .. })),
            "missing tombstone at /a: {:?}",
            dirents
        );
        assert!(
            dirents
                .iter()
                .any(|(p, c)| p == "/b" && matches!(c, Dstate::Tombstone { .. })),
            "missing tombstone at /b: {:?}",
            dirents
        );
        assert!(dirents.iter().any(|(p, c)| p == "/c" && matches!(c, Dstate::BasePath { src: from, .. } if from == "/a")),
            "/c should be Link from /a: {:?}", dirents);
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
                .any(|(p, c)| p == "/a" && matches!(c, Dstate::Tombstone { .. })),
            "missing tombstone at /a: {:?}",
            dirents
        );
        assert!(dirents.iter().any(|(p, c)| p == "/c" && matches!(c, Dstate::BasePath { src: from, .. } if from == "/a")),
            "/c should be Link from /a: {:?}", dirents);
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
                    Dstate::StagedInode {
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
        // R(/other, /a/b) renames intermediate /a/b (which is a Dir(Passthrough, ...) node).
        let tree = build(&[add("/a/b/c/file", 1), rename_dir("/other", "/a/b")]);
        let dirents = tree.into_dirents();
        let paths: Vec<_> = dirents.iter().map(|(p, _)| p.as_str()).collect();
        // /a/b was an intermediate dir → source_had_base=true → Tombstone at /a/b.
        assert!(
            dirents
                .iter()
                .any(|(p, c)| p == "/a/b" && matches!(c, Dstate::Tombstone { .. })),
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
                .any(|(p, c)| p == "/other/c/file" && matches!(c, Dstate::StagedInode { ino: 1, .. })),
            "/other/c/file should be Inode(ino=1): {:?}",
            dirents
        );
        // /other should have a Link dirent (from intermediate dir rename)
        assert!(
            dirents.iter().any(|(p, c)| p == "/other"
                && matches!(c, Dstate::BasePath { src: from, .. } if from == "/a/b")),
            "/other should be Link from /a/b: {:?}",
            dirents
        );
    }

    #[test]
    fn tombstone_dtype_returns_stored_value() {
        assert_eq!(Dstate::Tombstone { dtype: libc::DT_REG }.dtype(), libc::DT_REG);
        assert_eq!(Dstate::Tombstone { dtype: libc::DT_DIR }.dtype(), libc::DT_DIR);
        assert_eq!(Dstate::Tombstone { dtype: libc::DT_LNK }.dtype(), libc::DT_LNK);
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
        let dirents = cs.into_dirents();
        // Both /a and /b should be tombstoned.
        assert!(
            dirents.iter().any(|(p, d)| p == "/a" && matches!(d, Dstate::Tombstone { .. })),
            "/a should be Tombstone: {dirents:?}"
        );
        assert!(
            dirents.iter().any(|(p, d)| p == "/b" && matches!(d, Dstate::Tombstone { .. })),
            "/b should be Tombstone (base content): {dirents:?}"
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
        expected.push(1); // has_dirent
        // PackedDstate: dtype=File(4) at [62:60], in_base=false, ino=1, gen=0
        let packed: u64 = (dtype_pack(libc::DT_REG) << 60) | (1u64 << 16);
        expected.extend_from_slice(&packed.to_le_bytes());
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
        expected.push(1); // has_dirent
        // Tombstone: dtype=File(4) at [62:60], in_base=1 at [59], ino=0
        let packed: u64 = (dtype_pack(libc::DT_REG) << 60) | (1u64 << 59);
        expected.extend_from_slice(&packed.to_le_bytes());
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
        // Should have 2 children: "a.txt" (tombstone) and "b.txt" (link)
        let mut cursor = 0usize;
        let child_count = u16::from_le_bytes([buf[cursor], buf[cursor + 1]]);
        cursor += 2;
        assert_eq!(child_count, 2);

        // Children are sorted: "a.txt" before "b.txt"
        // Node 1: "a.txt" — tombstone
        let name_len = u16::from_le_bytes([buf[cursor], buf[cursor + 1]]) as usize;
        cursor += 2;
        assert_eq!(&buf[cursor..cursor + name_len], b"a.txt");
        cursor += name_len;
        assert_eq!(buf[cursor], 1); // has_dirent
        cursor += 1;
        let packed = u64::from_le_bytes(buf[cursor..cursor + 8].try_into().unwrap());
        // Tombstone: dtype=File(4) at [62:60], in_base=1 at [59]
        assert_eq!(packed, (dtype_pack(libc::DT_REG) << 60) | (1u64 << 59));
        cursor += 8;
        let cc = u16::from_le_bytes([buf[cursor], buf[cursor + 1]]);
        cursor += 2;
        assert_eq!(cc, 0);

        // Node 2: "b.txt" — link to /a.txt
        let name_len = u16::from_le_bytes([buf[cursor], buf[cursor + 1]]) as usize;
        cursor += 2;
        assert_eq!(&buf[cursor..cursor + name_len], b"b.txt");
        cursor += name_len;
        assert_eq!(buf[cursor], 1); // has_dirent
        cursor += 1;
        let packed = u64::from_le_bytes(buf[cursor..cursor + 8].try_into().unwrap());
        // Link: bit 63 set, dtype=File(4) at [62:60], in_base=false (Rename → dest not in base)
        assert!((packed as i64) < 0, "link should have bit 63 set");
        cursor += 8;
        // Trailing: base_len + base_path + NUL
        let base_len = u16::from_le_bytes([buf[cursor], buf[cursor + 1]]) as usize;
        cursor += 2;
        assert_eq!(&buf[cursor..cursor + base_len], b"/a.txt");
        cursor += base_len;
        assert_eq!(buf[cursor], 0); // NUL
        cursor += 1;
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

        // Node "dir": name_len=3, name="dir", has_dirent=1, packed(Dir,ino=10), child_count=1
        let name_len = u16::from_le_bytes([buf[cursor], buf[cursor + 1]]) as usize;
        cursor += 2;
        assert_eq!(&buf[cursor..cursor + name_len], b"dir");
        cursor += name_len;
        assert_eq!(buf[cursor], 1); // has_dirent
        cursor += 1;
        let packed = u64::from_le_bytes(buf[cursor..cursor + 8].try_into().unwrap());
        // Dir inode: dtype=Dir(1) at bits [62:61], in_base=false, ino=10
        assert_eq!((packed >> 61) & 3, 1); // dtype=Dir
        assert_eq!((packed >> 16) & 0xFFFFFFFF, 10); // ino
        cursor += 8;

        // child_count for "dir" subtree = 1
        let cc = u16::from_le_bytes([buf[cursor], buf[cursor + 1]]);
        cursor += 2;
        assert_eq!(cc, 1);

        // Node "file": name_len=4, name="file", has_dirent=1, packed(File,ino=20), child_count=0
        let name_len = u16::from_le_bytes([buf[cursor], buf[cursor + 1]]) as usize;
        cursor += 2;
        assert_eq!(&buf[cursor..cursor + name_len], b"file");
        cursor += name_len;
        assert_eq!(buf[cursor], 1); // has_dirent
        cursor += 1;
        let packed = u64::from_le_bytes(buf[cursor..cursor + 8].try_into().unwrap());
        assert_eq!((packed >> 16) & 0xFFFFFFFF, 20); // ino
        cursor += 8;
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

        // Node "dir": has_dirent=0
        let name_len = u16::from_le_bytes([buf[cursor], buf[cursor + 1]]) as usize;
        cursor += 2;
        assert_eq!(&buf[cursor..cursor + name_len], b"dir");
        cursor += name_len;
        assert_eq!(buf[cursor], 0); // no dirent
        cursor += 1;

        // child_count = 1
        let cc = u16::from_le_bytes([buf[cursor], buf[cursor + 1]]);
        cursor += 2;
        assert_eq!(cc, 1);

        // Node "file": has_dirent=1
        let name_len = u16::from_le_bytes([buf[cursor], buf[cursor + 1]]) as usize;
        cursor += 2;
        assert_eq!(&buf[cursor..cursor + name_len], b"file");
        cursor += name_len;
        assert_eq!(buf[cursor], 1); // has_dirent
        cursor += 1;
        cursor += 8; // packed
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

        // Read names in order
        let mut names = Vec::new();
        for _ in 0..3 {
            let name_len = u16::from_le_bytes([buf[cursor], buf[cursor + 1]]) as usize;
            cursor += 2;
            names.push(std::str::from_utf8(&buf[cursor..cursor + name_len]).unwrap().to_string());
            cursor += name_len;
            cursor += 1; // has_dirent
            cursor += 8; // packed
            cursor += 2; // child_count
        }
        assert_eq!(names, vec!["a", "m", "z"]);
    }

    #[test]
    fn serialize_passthrough_dir_empty_subtree_omitted() {
        // Create a tree with an passthrough dir that has an empty subtree
        let mut tree = DirTree::new();
        tree.nodes.insert(
            "empty".to_string(),
            DirNode::Dir(Dstate::Passthrough, DirTree::new()),
        );
        let buf = tree.serialize();
        // Should produce just child_count=0 (the empty passthrough dir is skipped)
        assert_eq!(buf, vec![0x00, 0x00]);
    }

    #[test]
    fn serialize_stale_intermediates_after_cancel() {
        // Add a deeply nested file then delete it.  The cancel removes the
        // leaf File node but leaves Dir(Passthrough) intermediates.  The leaf-level
        // empty dir is filtered, but upper intermediates remain in the
        // serialized output.  The kernel tolerates these (skips nodes with
        // has_dirent=0 and child_count=0).
        let tree = build(&[add("/a/b/c/file", 1), delete("/a/b/c/file")]);
        assert_eq!(tree.len(), 0, "no dirents after cancel");
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
        // Verify the packed bit layout for an inode dirent
        let tree = build(&[Action::Modify {
            path: "/f".into(),
            ino: 42,
            dtype: Some(libc::DT_LNK),
        }]);
        let buf = tree.serialize();
        // Skip: root child_count(2) + name_len(2) + name(1) + has_dirent(1)
        let packed = u64::from_le_bytes(buf[6..14].try_into().unwrap());
        // [63]=0, [62:60]=5(Link), [59]=1(in_base=true for Modify), [47:16]=42, [15:0]=0
        assert_eq!((packed >> 63) & 1, 0); // not a link pde
        assert_eq!((packed >> 60) & 7, dtype_pack(libc::DT_LNK)); // dtype=Link
        assert_eq!((packed >> 59) & 1, 1); // in_base=true
        assert_eq!((packed >> 16) & 0xFFFFFFFF, 42); // ino
        assert_eq!(packed & 0xFFFF, 0); // gen bits zeroed
    }

    #[test]
    fn serialize_link_bits_correct() {
        // Verify the packed bit layout for a link dirent
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
        assert_eq!(buf[cursor], 1); // has_dirent
        cursor += 1;
        let packed = u64::from_le_bytes(buf[cursor..cursor + 8].try_into().unwrap());
        cursor += 8;
        // [63]=1, [62:60]=2(Dir), [59]=0(Rename: dest not in base), [58:0]=0
        assert_eq!((packed >> 63) & 1, 1); // link
        assert_eq!((packed >> 60) & 7, dtype_pack(libc::DT_DIR)); // dtype=Dir
        assert_eq!((packed >> 59) & 1, 0); // in_base=false for Rename
        assert_eq!(packed & 0x07FFFFFFFFFFFFFF, 0); // pointer bits zeroed

        // Trailing base_path
        let base_len = u16::from_le_bytes([buf[cursor], buf[cursor + 1]]) as usize;
        cursor += 2;
        assert_eq!(&buf[cursor..cursor + base_len], b"/src");
        cursor += base_len;
        assert_eq!(buf[cursor], 0); // NUL
    }

    #[test]
    fn dtype_to_packed() {
        assert_eq!(dtype_pack(libc::DT_REG), 4);
        assert_eq!(dtype_pack(libc::DT_DIR), 2);
        assert_eq!(dtype_pack(libc::DT_LNK), 5);
    }

    #[test]
    fn serialize_passthrough_file_omitted() {
        // An Passthrough file node should be skipped entirely during serialization.
        let mut tree = DirTree::new();
        tree.nodes.insert(
            "ghost".to_string(),
            DirNode::File(Dstate::Passthrough),
        );
        let buf = tree.serialize();
        assert_eq!(buf, vec![0x00, 0x00], "Passthrough file should be omitted");
    }

    #[test]
    fn serialize_passthrough_dir_no_dirent() {
        // An Passthrough dir with children should serialize has_dirent=0
        // but still emit the subtree.
        let mut tree = DirTree::new();
        let mut sub = DirTree::new();
        sub.nodes.insert(
            "child".to_string(),
            DirNode::File(Dstate::StagedInode {
                ino: 1,
                dtype: libc::DT_REG,
                in_base: false,
            }),
        );
        tree.nodes.insert(
            "dir".to_string(),
            DirNode::Dir(Dstate::Passthrough, sub),
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
        // has_dirent = 0 (Passthrough)
        assert_eq!(buf[cursor], 0, "Passthrough dir should have has_dirent=0");
        cursor += 1;
        // subtree child_count = 1 (the child)
        assert_eq!(u16::from_le_bytes([buf[cursor], buf[cursor + 1]]), 1);
    }

    #[test]
    fn serialize_tombstone_dir_dtype() {
        // delete_dir on a base-only dir produces a tombstone with DT_DIR.
        let tree = build(&[delete_dir("/d")]);
        let buf = tree.serialize();
        // Skip root child_count(2) + name_len(2) + name(1) + has_dirent(1)
        let packed = u64::from_le_bytes(buf[6..14].try_into().unwrap());
        // Tombstone: dtype=Dir at [62:60], in_base=1 at [59], ino=0
        assert_eq!((packed >> 60) & 7, dtype_pack(libc::DT_DIR));
        assert_eq!((packed >> 59) & 1, 1);
        assert_eq!((packed >> 16) & 0xFFFFFFFF, 0);
    }

    #[test]
    fn serialize_tombstone_symlink_dtype() {
        // delete on a base-only symlink produces a tombstone with DT_LNK.
        let tree = build(&[Action::Delete {
            path: "/s".into(),
            dtype: Some(libc::DT_LNK),
        }]);
        let buf = tree.serialize();
        let packed = u64::from_le_bytes(buf[6..14].try_into().unwrap());
        assert_eq!((packed >> 60) & 7, dtype_pack(libc::DT_LNK));
        assert_eq!((packed >> 59) & 1, 1);
        assert_eq!((packed >> 16) & 0xFFFFFFFF, 0);
    }

    #[test]
    fn roundtrip_rename_dir_preserves_subtree() {
        // Rename dir with children back to original — children should survive.
        // Passthrough dir nodes are skipped by into_dirents, so /a won't appear.
        let tree = build(&[
            add("/a/child", 1),
            rename_dir("/tmp", "/a"),
            rename_dir("/a", "/tmp"),
        ]);
        assert_eq!(tree.len(), 1, "only the child is a staged change");
        let dirents = tree.into_dirents();
        assert!(
            dirents.iter().any(|(p, c)| p == "/a/child"
                && matches!(c, Dstate::StagedInode { ino: 1, .. })),
            "/a/child should survive roundtrip: {:?}",
            dirents
        );
    }

    #[test]
    #[should_panic(expected = "base_path too long")]
    fn serialize_rejects_oversized_src() {
        let mut tree = DirTree::new();
        tree.nodes.insert(
            "link".to_string(),
            DirNode::File(Dstate::BasePath {
                src: "a".repeat(u16::MAX as usize + 1),
                dtype: libc::DT_REG,
                in_base: false,
            }),
        );
        tree.serialize();
    }
}

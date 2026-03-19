// agfs CLI — journal/resolve.rs
//
// Resolve (replay) the append-only journal into a list of Changes.
//
// Intermediate operations collapse into their final effect:
// - `Added(x) → Deleted(x)` cancels out (x was new).
// - `Deleted(old) + Redirect(new, old)` collapses to `Rename(old→new)`.
// - Redirect chains collapse: `Redirect(b, a)` then `Redirect(c, b)` → `Rename(a, c)`.
// - Multiple records for the same path keep only the final state.

use super::types::{Checkpoint, DType, Record};
use anyhow::Result;
use std::collections::BTreeMap;

/// A resolved change — the final effect of replaying the journal.
#[derive(Debug)]
pub enum Change {
    Added {
        path: String,
        ino: u64,
        dtype: DType,
    },
    Modified {
        path: String,
        ino: u64,
        dtype: DType,
    },
    Deleted(String),
    Renamed {
        from: String,
        to: String,
        dtype: DType,
    },
    Replaced {
        from: String,
        to: String,
        dtype: DType,
    },
}

impl Change {
    /// Return the staged inode ID if this change carries one.
    pub fn ino(&self) -> Option<u64> {
        match self {
            Change::Added { ino, .. } | Change::Modified { ino, .. } => Some(*ino),
            _ => None,
        }
    }

    /// True if this change involves the given path (as source or destination).
    pub fn matches_path(&self, target: &str) -> bool {
        match self {
            Change::Added { path, .. } | Change::Modified { path, .. } => path == target,
            Change::Deleted(path) => path == target,
            Change::Renamed { from, to, .. } | Change::Replaced { from, to, .. } => {
                from == target || to == target
            }
        }
    }
}

/// Resolve records into their final collapsed changes.
pub fn resolve(records: Vec<Record>) -> Result<Vec<Change>> {
    let mut r = Resolver::new();
    for record in records {
        r.process(record);
    }
    Ok(r.into_changes())
}



/// Incremental resolution state — processes records one at a time.
/// Each path maps to one `Action` describing what commit should do.
///
/// Public so callers can drive iteration for single-pass processing
/// (e.g. snapshot at a checkpoint, then continue to the end).
#[derive(Clone, Default)]
pub struct Resolver {
    state: BTreeMap<String, Action>,
}

#[derive(Clone, Debug, PartialEq)]
enum Action {
    Added { ino: u64, dtype: DType },
    Modified { ino: u64, dtype: DType },
    Deleted,
    Renamed { origin: String, dtype: DType },
    Replaced { origin: String, dtype: DType },
}

impl Resolver {
    pub fn new() -> Self {
        Self::default()
    }

    fn process_stage(&mut self, path: String, dtype: Option<DType>, ino: u64, in_base: bool) {
        let dt = dtype.unwrap_or(DType::File);
        match self.state.remove(&path) {
            Some(Action::Renamed { origin, .. } | Action::Replaced { origin, .. }) => {
                // Rename + modify at same path: decompose into
                // delete(origin) + modified(path).
                self.state.insert(origin, Action::Deleted);
                self.state.insert(path, Action::Modified { ino, dtype: dt });
            }
            Some(Action::Deleted) => {
                self.state.insert(
                    path,
                    Action::Modified { ino, dtype: dt },
                );
            }
            _ => {
                if in_base {
                    self.state.insert(path, Action::Modified { ino, dtype: dt });
                } else {
                    self.state.insert(path, Action::Added { ino, dtype: dt });
                }
            }
        }
    }

    fn process_redirect(&mut self, path: String, dtype: Option<DType>, base_path: String, in_base: bool) {
        let dt = dtype.unwrap_or(DType::File);

        // Overwrite whatever was at the destination.
        // If a prior redirect existed, its origin becomes a Delete.
        // Other prior states are silently replaced — the R/P record
        // from the kernel is authoritative.
        if let Some(Action::Renamed { origin, .. } | Action::Replaced { origin, .. }) =
            self.state.remove(&path)
        {
            self.state.insert(origin, Action::Deleted);
        }

        // The kernel emits D(old) before R/P(new, old).
        // Remove that spurious delete.
        if matches!(self.state.get(&base_path), Some(Action::Deleted)) {
            self.state.remove(&base_path);
        }

        // If the source was staged, decompose: emit delete for base
        // files and preserve the dtype from the staged action.
        let final_dt = match self.state.remove(&base_path) {
            Some(Action::Modified { dtype: prev_dt, .. }) => {
                self.state.insert(base_path.clone(), Action::Deleted);
                prev_dt
            }
            Some(Action::Added { dtype: prev_dt, .. }) => prev_dt,
            _ => dt,
        };

        if in_base {
            self.state.insert(
                path,
                Action::Replaced { origin: base_path, dtype: final_dt },
            );
        } else {
            self.state.insert(
                path,
                Action::Renamed { origin: base_path, dtype: final_dt },
            );
        }
    }

    pub fn process(&mut self, record: Record) {
        match record {
            Record::Added { path, dtype, ino } => {
                self.process_stage(path, dtype, ino, false);
            }
            Record::Modified { path, dtype, ino } => {
                self.process_stage(path, dtype, ino, true);
            }
            Record::Deleted { path } => match self.state.remove(&path) {
                Some(Action::Added { .. }) => {
                    // Staged-only file deleted — cancels out.
                }
                Some(Action::Renamed { origin, .. }) => {
                    self.state.insert(origin, Action::Deleted);
                }
                Some(Action::Replaced { origin, .. }) => {
                    self.state.insert(origin, Action::Deleted);
                    self.state.insert(path, Action::Deleted);
                }
                _ => {
                    self.state.insert(path, Action::Deleted);
                }
            },
            Record::Redirect {
                path,
                dtype,
                base: base_path,
            } => {
                self.process_redirect(path, dtype, base_path, false);
            }
            Record::Replace {
                path,
                dtype,
                base: base_path,
            } => {
                self.process_redirect(path, dtype, base_path, true);
            }
            Record::Checkpoint(_) | Record::Restore { .. } => {}
        }
    }

    /// Consume the state and produce the final change list.
    /// Order: renames, then adds/modifies, then deletes.
    pub fn into_changes(self) -> Vec<Change> {
        let mut changes = Vec::new();
        for (path, action) in self.state {
            emit_action(&mut changes, path, action);
        }
        changes.sort_by_key(|c| match c {
            Change::Renamed { .. } | Change::Replaced { .. } => 0,
            Change::Added { .. } | Change::Modified { .. } => 1,
            Change::Deleted(_) => 2,
        });
        changes
    }
}

/// A group of resolved changes between two checkpoint boundaries.
#[derive(Debug)]
pub struct ResolvedSegment {
    /// The checkpoint at the start of this segment.
    pub from: Checkpoint,
    /// The checkpoint at the end, or None for trailing (unsaved) changes.
    pub to: Option<Checkpoint>,
    pub changes: Vec<Change>,
}

/// Resolve the journal into segments grouped by checkpoint boundaries.
///
/// Each segment contains the *delta* of changes introduced between its
/// `from` and `to` checkpoints.  The trailing segment (to=None) holds
/// unsaved changes after the last checkpoint.
///
/// Records before the first checkpoint are skipped (the initial checkpoint
/// is always created at mount time).
pub fn resolve_segments(records: Vec<Record>) -> Result<Vec<ResolvedSegment>> {
    // Collect checkpoint boundary indices.
    let chk_indices: Vec<usize> = records
        .iter()
        .enumerate()
        .filter_map(|(i, r)| match r {
            Record::Checkpoint(_) => Some(i),
            _ => None,
        })
        .collect();

    if chk_indices.len() < 2 {
        // 0 checkpoints: nothing to show.
        // 1 checkpoint: only the initial checkpoint, no segments between pairs.
        // In either case, there may be trailing changes.
        let from = if let Some(&idx) = chk_indices.first() {
            match &records[idx] {
                Record::Checkpoint(c) => c.clone(),
                _ => unreachable!(),
            }
        } else {
            return Ok(vec![]);
        };

        let trailing: Vec<Record> = records
            .into_iter()
            .skip(chk_indices[0] + 1)
            .filter(|r| !matches!(r, Record::Checkpoint(_) | Record::Restore { .. }))
            .collect();
        if trailing.is_empty() {
            return Ok(vec![]);
        }
        let changes = resolve(trailing)?;
        return Ok(vec![ResolvedSegment {
            from,
            to: None,
            changes,
        }]);
    }

    // Resolve each segment between consecutive checkpoint pairs.
    let mut segments = Vec::new();
    let mut records_iter = records.into_iter();
    let mut record_idx = 0;

    // Skip records before the first checkpoint, extract it.
    let first_chk_idx = chk_indices[0];
    let mut prev_chk = None;
    while record_idx <= first_chk_idx {
        let record = records_iter.next().unwrap();
        if let Record::Checkpoint(c) = record {
            prev_chk = Some(c);
        }
        record_idx += 1;
    }

    // Build a segment for each consecutive pair.
    for &chk_idx in &chk_indices[1..] {
        let mut resolver = Resolver::new();
        let mut cur_chk = None;
        while record_idx <= chk_idx {
            let record = records_iter.next().unwrap();
            if let Record::Checkpoint(c) = record {
                cur_chk = Some(c);
            } else {
                resolver.process(record);
            }
            record_idx += 1;
        }
        segments.push(ResolvedSegment {
            from: prev_chk.clone().unwrap(),
            to: cur_chk.clone(),
            changes: resolver.into_changes(),
        });
        prev_chk = cur_chk;
    }

    // Trailing changes after the last checkpoint.
    let mut resolver = Resolver::new();
    for record in records_iter {
        resolver.process(record);
    }
    let trailing = resolver.into_changes();
    if !trailing.is_empty() {
        segments.push(ResolvedSegment {
            from: prev_chk.unwrap(),
            to: None,
            changes: trailing,
        });
    }

    Ok(segments)
}

/// Convert a (path, action) pair into Changes.
fn emit_action(out: &mut Vec<Change>, path: String, action: Action) {
    match action {
        Action::Added { ino, dtype } => {
            out.push(Change::Added { path, ino, dtype });
        }
        Action::Modified { ino, dtype } => {
            out.push(Change::Modified { path, ino, dtype });
        }
        Action::Deleted => {
            out.push(Change::Deleted(path));
        }
        Action::Renamed { origin, dtype } => {
            if origin != path {
                out.push(Change::Renamed { from: origin, to: path, dtype });
            }
        }
        Action::Replaced { origin, dtype } => {
            if origin != path {
                out.push(Change::Replaced { from: origin, to: path, dtype });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::types::{Checkpoint, DType, Record};
    use super::super::timeline::{reachable, find_checkpoint_index, slice_records};
    use std::fs;
    use std::path::Path;
    fn setup_test_dir() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("inodes")).unwrap();
        dir
    }

    fn read(dir: &Path) -> Vec<Record> {
        crate::journal::read(dir).unwrap().records
    }

    #[test]
    fn resolve_empty() {
        let dir = setup_test_dir();
        let changes = resolve(read(dir.path())).unwrap();
        assert!(changes.is_empty());
    }

    #[test]
    fn resolve_add() {
        let dir = setup_test_dir();
        let mut data = Vec::new();
        data.extend_from_slice(b"A\0/nonexistent_test_12345\0new.txt\0f\01\n");
        fs::write(dir.path().join("journal"), &data).unwrap();
        fs::write(dir.path().join("inodes/1"), "content").unwrap();

        let changes = resolve(read(dir.path())).unwrap();
        assert_eq!(changes.len(), 1);
        assert!(matches!(&changes[0], Change::Added { path, .. } if path.contains("new.txt")));
    }

    #[test]
    fn resolve_delete() {
        let dir = setup_test_dir();
        let mut data = Vec::new();
        data.extend_from_slice(b"D\0/etc\0hostname\n");
        fs::write(dir.path().join("journal"), &data).unwrap();

        let changes = resolve(read(dir.path())).unwrap();
        assert_eq!(changes.len(), 1);
        assert!(matches!(&changes[0], Change::Deleted(p) if p.contains("hostname")));
    }

    #[test]
    fn resolve_modify() {
        let dir = setup_test_dir();
        let mut data = Vec::new();
        data.extend_from_slice(b"M\0/etc\0hostname\0f\01\n");
        fs::write(dir.path().join("journal"), &data).unwrap();
        fs::write(dir.path().join("inodes/1"), "modified").unwrap();

        let changes = resolve(read(dir.path())).unwrap();
        assert_eq!(changes.len(), 1);
        assert!(
            matches!(&changes[0], Change::Modified { path, ino: 1, dtype: DType::File }
                if path == "/etc/hostname"),
            "expected Modified(/etc/hostname, ino=1), got: {:?}",
            changes[0]
        );
    }

    #[test]
    fn resolve_rename() {
        let dir = setup_test_dir();
        let mut data = Vec::new();
        // Rename: E(old, Deleted) + E(new, Redirect(old))
        data.extend_from_slice(b"D\0/old\0path\n");
        data.extend_from_slice(b"R\0/new\0path\0f\0/old/path\n");
        fs::write(dir.path().join("journal"), &data).unwrap();

        let changes = resolve(read(dir.path())).unwrap();
        assert_eq!(changes.len(), 1);
        assert!(matches!(&changes[0], Change::Renamed { from, to, .. }
            if from == "/old/path" && to == "/new/path"));
    }

    /// Redirect-based rename must preserve the dtype from the E record.
    #[test]
    fn resolve_rename_preserves_dtype() {
        let dir = setup_test_dir();
        let mut data = Vec::new();
        // Rename directory: E(old, Deleted) + E(new, Redirect(old), dtype=d)
        data.extend_from_slice(b"D\0\0olddir\n");
        data.extend_from_slice(b"R\0\0newdir\0d\0/olddir\n");
        fs::write(dir.path().join("journal"), &data).unwrap();

        let changes = resolve(read(dir.path())).unwrap();
        assert_eq!(changes.len(), 1);
        assert!(
            matches!(&changes[0], Change::Renamed { from, to, dtype: DType::Dir, .. }
                if from == "/olddir" && to == "/newdir"),
            "expected Renamed with DType::Dir, got: {:?}",
            changes[0]
        );
    }

    /// Chained directory renames must preserve dtype through the chain.
    #[test]
    fn chained_rename_preserves_dtype() {
        let dir = setup_test_dir();
        let mut data = Vec::new();
        // Rename dir a->b: delete a + redirect b->a (dtype=d)
        data.extend_from_slice(b"D\0\0a\n");
        data.extend_from_slice(b"R\0\0b\0d\0/a\n");
        // Rename dir b->c: delete b + redirect c->a (dtype=d, kernel follows chain)
        data.extend_from_slice(b"D\0\0b\n");
        data.extend_from_slice(b"R\0\0c\0d\0/a\n");
        fs::write(dir.path().join("journal"), &data).unwrap();

        let changes = resolve(read(dir.path())).unwrap();
        assert_eq!(changes.len(), 1);
        assert!(
            matches!(&changes[0], Change::Renamed { from, to, dtype: DType::Dir, .. }
                if from == "/a" && to == "/c"),
            "expected Renamed(a->c) with DType::Dir, got: {:?}",
            changes[0]
        );
    }

    /// Rename a base file then modify at the new path: produces Deleted + Modified.
    #[test]
    fn rename_then_modify_produces_delete_modified() {
        let dir = setup_test_dir();
        let mut data = Vec::new();
        // Rename base file: E(old, Deleted) + E(new, Redirect(old))
        data.extend_from_slice(b"D\0\0old.txt\n");
        data.extend_from_slice(b"R\0\0new.txt\0f\0/old.txt\n");
        // Modify at new path (COW): M(new, ino=5)
        data.extend_from_slice(b"M\0\0new.txt\0f\05\n");
        fs::write(dir.path().join("journal"), &data).unwrap();
        fs::write(dir.path().join("inodes/5"), "modified").unwrap();

        let changes = resolve(read(dir.path())).unwrap();
        assert_eq!(changes.len(), 2, "expected 2 changes, got: {changes:?}");
        let has_deleted = changes
            .iter()
            .any(|c| matches!(c, Change::Deleted(p) if p == "/old.txt"));
        let has_modified = changes.iter().any(|c| {
            matches!(c, Change::Modified { path, ino: 5, .. }
                if path == "/new.txt")
        });
        assert!(has_deleted, "expected Deleted(/old.txt), got: {changes:?}");
        assert!(has_modified, "expected Modified(/new.txt, ino=5), got: {changes:?}");
    }

    /// touch x, mv x->y: staging-created file renamed.
    /// Should collapse to a single Add at the new path, not a base rename.
    #[test]
    fn create_then_rename_collapses_to_add() {
        let dir = setup_test_dir();
        let mut data = Vec::new();
        // Create x (staged)
        data.extend_from_slice(b"A\0/nonexistent_test_12345\0x\0f\01\n");
        // Rename x->y: kernel emits staged rename (same ino at new path + delete old)
        data.extend_from_slice(b"D\0/nonexistent_test_12345\0x\n");
        data.extend_from_slice(b"A\0/nonexistent_test_12345\0y\0f\01\n");
        fs::write(dir.path().join("journal"), &data).unwrap();
        fs::write(dir.path().join("inodes/1"), "content").unwrap();

        let changes = resolve(read(dir.path())).unwrap();
        assert_eq!(changes.len(), 1, "expected 1 change, got: {changes:?}");
        assert!(
            matches!(&changes[0], Change::Added { path, ino, .. } if path == "/nonexistent_test_12345/y" && *ino == 1),
            "expected Added at y with ino 1, got: {:?}",
            changes[0]
        );
    }

    /// mv a->b, touch a: rename then recreate at old path.
    /// Should produce Renamed(a->b) + Added(a).
    #[test]
    fn rename_then_recreate_at_old_path() {
        let dir = setup_test_dir();
        let mut data = Vec::new();
        // Rename a->b (redirect): delete old + redirect new
        data.extend_from_slice(b"D\0/nonexistent_test_12345\0a\n");
        data.extend_from_slice(b"R\0/nonexistent_test_12345\0b\0f\0/nonexistent_test_12345/a\n");
        // Create new file at a
        data.extend_from_slice(b"A\0/nonexistent_test_12345\0a\0f\02\n");
        fs::write(dir.path().join("journal"), &data).unwrap();
        fs::write(dir.path().join("inodes/2"), "new content").unwrap();

        let changes = resolve(read(dir.path())).unwrap();
        assert_eq!(changes.len(), 2, "expected 2 changes, got: {changes:?}");
        let has_rename = changes.iter().any(|c| {
            matches!(c, Change::Renamed { from, to, .. }
            if from == "/nonexistent_test_12345/a" && to == "/nonexistent_test_12345/b")
        });
        let has_add = changes.iter().any(|c| {
            matches!(c, Change::Added { path, ino, .. }
            if path == "/nonexistent_test_12345/a" && *ino == 2)
        });
        assert!(has_rename, "expected Renamed(a->b), got: {changes:?}");
        assert!(has_add, "expected Added(a, 2), got: {changes:?}");
    }

    /// mv a->b, mv b->c: chained renames.
    /// Should collapse to a single Renamed(a->c).
    #[test]
    fn chained_renames_collapse() {
        let dir = setup_test_dir();
        let mut data = Vec::new();
        // Rename a->b: delete a + redirect b->a
        data.extend_from_slice(b"D\0/nonexistent_test_12345\0a\n");
        data.extend_from_slice(b"R\0/nonexistent_test_12345\0b\0f\0/nonexistent_test_12345/a\n");
        // Rename b->c: delete b + redirect c->a (kernel follows the chain)
        data.extend_from_slice(b"D\0/nonexistent_test_12345\0b\n");
        data.extend_from_slice(b"R\0/nonexistent_test_12345\0c\0f\0/nonexistent_test_12345/a\n");
        fs::write(dir.path().join("journal"), &data).unwrap();

        let changes = resolve(read(dir.path())).unwrap();
        assert_eq!(changes.len(), 1, "expected 1 change, got: {changes:?}");
        assert!(
            matches!(&changes[0], Change::Renamed { from, to, .. }
                if from == "/nonexistent_test_12345/a" && to == "/nonexistent_test_12345/c"),
            "expected Renamed(a->c), got: {:?}",
            changes[0]
        );
    }

    /// mv a->b, mv b->a: rename back to original.
    /// Should cancel out (no net change).
    #[test]
    fn rename_back_and_forth_cancels() {
        let dir = setup_test_dir();
        let mut data = Vec::new();
        // Rename a->b: delete a + redirect b->a
        data.extend_from_slice(b"D\0/nonexistent_test_12345\0a\n");
        data.extend_from_slice(b"R\0/nonexistent_test_12345\0b\0f\0/nonexistent_test_12345/a\n");
        // Rename b->a: delete b + redirect a->a (kernel follows chain: b's base_path is a)
        data.extend_from_slice(b"D\0/nonexistent_test_12345\0b\n");
        data.extend_from_slice(b"R\0/nonexistent_test_12345\0a\0f\0/nonexistent_test_12345/a\n");
        fs::write(dir.path().join("journal"), &data).unwrap();

        let changes = resolve(read(dir.path())).unwrap();
        assert!(
            changes.is_empty(),
            "rename back and forth should cancel out, got: {changes:?}"
        );
    }

    /// Rename onto an existing base file (overwrite).
    /// Should produce Renamed(src->dst) + Deleted(dst originally).
    /// The resolver sees: Delete(src) + Redirect(dst, base=src).
    /// Since dst existed in base, it's implicitly overwritten by the rename.
    #[test]
    fn rename_overwrite_base_file() {
        let dir = setup_test_dir();
        let mut data = Vec::new();
        // /etc/hostname and /etc/hosts both exist in base
        // Rename hostname -> hosts (overwrite)
        data.extend_from_slice(b"D\0/etc\0hostname\n");
        data.extend_from_slice(b"R\0/etc\0hosts\0f\0/etc/hostname\n");
        fs::write(dir.path().join("journal"), &data).unwrap();

        let changes = resolve(read(dir.path())).unwrap();
        // Should have a rename from hostname -> hosts
        let has_rename = changes.iter().any(|c| {
            matches!(c, Change::Renamed { from, to, .. }
            if from == "/etc/hostname" && to == "/etc/hosts")
        });
        assert!(
            has_rename,
            "expected Renamed(hostname->hosts), got: {changes:?}"
        );
    }

    /// touch x (staged), then mv y→x (base file overwrites staged file).
    /// The redirect should evict the prior staged add at x.
    /// Should produce Renamed(y→x), not a spurious Modified with the orphaned ino.
    #[test]
    fn redirect_overwrites_prior_staged_add() {
        let dir = setup_test_dir();
        let mut data = Vec::new();
        // Stage x with ino 1
        data.extend_from_slice(b"A\0/nonexistent_test_12345\0x\0f\01\n");
        // Rename y→x (redirect): delete y + redirect x→y
        data.extend_from_slice(b"D\0/nonexistent_test_12345\0y\n");
        data.extend_from_slice(b"R\0/nonexistent_test_12345\0x\0f\0/nonexistent_test_12345/y\n");
        fs::write(dir.path().join("journal"), &data).unwrap();
        fs::write(dir.path().join("inodes/1"), "staged content").unwrap();

        let changes = resolve(read(dir.path())).unwrap();
        assert_eq!(changes.len(), 1, "expected 1 change, got: {changes:?}");
        assert!(
            matches!(&changes[0], Change::Renamed { from, to, .. }
                if from == "/nonexistent_test_12345/y" && to == "/nonexistent_test_12345/x"),
            "expected Renamed(y→x), got: {:?}",
            changes[0]
        );
    }

    /// mv a→b, then mv c→b (second redirect overwrites first rename destination).
    /// Should produce Renamed(c→b) + Deleted(a).
    #[test]
    fn redirect_overwrites_prior_rename_destination() {
        let dir = setup_test_dir();
        let mut data = Vec::new();
        // Rename a→b: delete a + redirect b→a
        data.extend_from_slice(b"D\0/nonexistent_test_12345\0a\n");
        data.extend_from_slice(b"R\0/nonexistent_test_12345\0b\0f\0/nonexistent_test_12345/a\n");
        // Rename c→b: delete c + redirect b→c (overwrites the a→b rename at b)
        data.extend_from_slice(b"D\0/nonexistent_test_12345\0c\n");
        data.extend_from_slice(b"R\0/nonexistent_test_12345\0b\0f\0/nonexistent_test_12345/c\n");
        fs::write(dir.path().join("journal"), &data).unwrap();

        let changes = resolve(read(dir.path())).unwrap();
        let has_rename = changes.iter().any(|c| {
            matches!(c, Change::Renamed { from, to, .. }
                if from == "/nonexistent_test_12345/c" && to == "/nonexistent_test_12345/b")
        });
        let has_delete = changes.iter().any(|c| {
            matches!(c, Change::Deleted(p)
                if p == "/nonexistent_test_12345/a")
        });
        assert!(has_rename, "expected Renamed(c→b), got: {changes:?}");
        assert!(has_delete, "expected Deleted(a), got: {changes:?}");
    }

    /// touch x, rm x: create then delete cancels out (x never existed in base).
    #[test]
    fn create_then_delete_cancels() {
        let dir = setup_test_dir();
        let mut data = Vec::new();
        data.extend_from_slice(b"A\0/nonexistent_test_12345\0x\0f\01\n");
        data.extend_from_slice(b"D\0/nonexistent_test_12345\0x\n");
        fs::write(dir.path().join("journal"), &data).unwrap();
        fs::write(dir.path().join("inodes/1"), "content").unwrap();

        let changes = resolve(read(dir.path())).unwrap();
        assert!(changes.is_empty(), "expected no changes, got: {changes:?}");
    }

    /// Modify a base file, then rename another base file onto it.
    /// The modification is overwritten — only the rename survives.
    #[test]
    fn redirect_overwrites_prior_modified() {
        let dir = setup_test_dir();
        let mut data = Vec::new();
        // COW modify /etc/hostname
        data.extend_from_slice(b"M\0/etc\0hostname\0f\01\n");
        // Rename /etc/hosts → /etc/hostname (overwrite the modified file)
        data.extend_from_slice(b"D\0/etc\0hosts\n");
        data.extend_from_slice(b"R\0/etc\0hostname\0f\0/etc/hosts\n");
        fs::write(dir.path().join("journal"), &data).unwrap();
        fs::write(dir.path().join("inodes/1"), "modified").unwrap();

        let changes = resolve(read(dir.path())).unwrap();
        assert_eq!(changes.len(), 1, "expected 1 change, got: {changes:?}");
        assert!(
            matches!(&changes[0], Change::Renamed { from, to, .. }
                if from == "/etc/hosts" && to == "/etc/hostname"),
            "expected Renamed(hosts→hostname), modification should be dropped, got: {:?}",
            changes[0]
        );
    }

    /// Multiple COW modifications to the same base file keep only the final ino.
    #[test]
    fn multiple_modifies_keep_final_ino() {
        let dir = setup_test_dir();
        let mut data = Vec::new();
        data.extend_from_slice(b"M\0/etc\0hostname\0f\01\n");
        data.extend_from_slice(b"M\0/etc\0hostname\0f\02\n");
        fs::write(dir.path().join("journal"), &data).unwrap();
        fs::write(dir.path().join("inodes/1"), "first").unwrap();
        fs::write(dir.path().join("inodes/2"), "second").unwrap();

        let changes = resolve(read(dir.path())).unwrap();
        assert_eq!(changes.len(), 1, "expected 1 change, got: {changes:?}");
        assert!(
            matches!(&changes[0], Change::Modified { path, ino: 2, .. }
                if path == "/etc/hostname"),
            "expected Modified with final ino=2, got: {:?}",
            changes[0]
        );
    }

    /// Delete a base file then create a new one at the same path.
    /// The Delete tells us the path existed, so the net result is Modified.
    #[test]
    fn delete_then_create_at_same_path() {
        let dir = setup_test_dir();
        let mut data = Vec::new();
        data.extend_from_slice(b"D\0/etc\0hostname\n");
        data.extend_from_slice(b"A\0/etc\0hostname\0f\01\n");
        fs::write(dir.path().join("journal"), &data).unwrap();
        fs::write(dir.path().join("inodes/1"), "replacement").unwrap();

        let changes = resolve(read(dir.path())).unwrap();
        assert_eq!(changes.len(), 1, "expected 1 change, got: {changes:?}");
        assert!(
            matches!(&changes[0], Change::Modified { path, ino: 1, .. }
                if path == "/etc/hostname"),
            "expected Modified (base file replaced), got: {:?}",
            changes[0]
        );
    }

    /// Delete a file then rename another file onto the deleted path.
    /// The explicit Delete at the destination is superseded by the redirect.
    #[test]
    fn delete_then_redirect_onto_same_path() {
        let dir = setup_test_dir();
        let mut data = Vec::new();
        // rm /etc/hostname
        data.extend_from_slice(b"D\0/etc\0hostname\n");
        // mv /etc/hosts → /etc/hostname: D(hosts) + R(hostname, hosts)
        data.extend_from_slice(b"D\0/etc\0hosts\n");
        data.extend_from_slice(b"R\0/etc\0hostname\0f\0/etc/hosts\n");
        fs::write(dir.path().join("journal"), &data).unwrap();

        let changes = resolve(read(dir.path())).unwrap();
        assert_eq!(changes.len(), 1, "expected 1 change, got: {changes:?}");
        assert!(
            matches!(&changes[0], Change::Renamed { from, to, .. }
                if from == "/etc/hosts" && to == "/etc/hostname"),
            "expected Renamed(hosts→hostname), got: {:?}",
            changes[0]
        );
    }

    /// Modify base file then delete: should still delete the base file.
    /// E(path, Staged) for a base file is a COW modify; E(path, Deleted) then means "delete base".
    #[test]
    fn modify_base_then_delete() {
        let dir = setup_test_dir();
        // /etc/hostname exists in base
        let mut data = Vec::new();
        data.extend_from_slice(b"M\0/etc\0hostname\0f\01\n");
        data.extend_from_slice(b"D\0/etc\0hostname\n");
        fs::write(dir.path().join("journal"), &data).unwrap();
        fs::write(dir.path().join("inodes/1"), "modified").unwrap();

        let changes = resolve(read(dir.path())).unwrap();
        assert_eq!(changes.len(), 1, "expected 1 change, got: {changes:?}");
        assert!(
            matches!(&changes[0], Change::Deleted(p) if p == "/etc/hostname"),
            "expected Deleted(/etc/hostname), got: {:?}",
            changes[0]
        );
    }

    /// Modify base file then rename: inode goes to new path, base old path deleted.
    #[test]
    fn modify_base_then_rename() {
        let dir = setup_test_dir();
        let mut data = Vec::new();
        // COW modify
        data.extend_from_slice(b"M\0/etc\0hostname\0f\01\n");
        // Staged rename: delete old + staged new (same ino)
        data.extend_from_slice(b"D\0/etc\0hostname\n");
        data.extend_from_slice(b"A\0/etc\0hostname.bak\0f\01\n");
        fs::write(dir.path().join("journal"), &data).unwrap();
        fs::write(dir.path().join("inodes/1"), "modified").unwrap();

        let changes = resolve(read(dir.path())).unwrap();
        // Should have: Add(/etc/hostname.bak, 1) + Delete(/etc/hostname)
        assert_eq!(changes.len(), 2, "expected 2 changes, got: {changes:?}");
        let has_add = changes.iter().any(|c| {
            matches!(c, Change::Added { path, ino, .. }
            if path == "/etc/hostname.bak" && *ino == 1)
        });
        let has_delete = changes
            .iter()
            .any(|c| matches!(c, Change::Deleted(p) if p == "/etc/hostname"));
        assert!(
            has_add,
            "expected Added(/etc/hostname.bak, 1), got: {changes:?}"
        );
        assert!(
            has_delete,
            "expected Deleted(/etc/hostname), got: {changes:?}"
        );
    }

    /// Rename that overwrites a base file, then destination is moved again.
    /// The overwritten base file must be deleted.
    ///
    /// Scenario: both /dir/a and /dir/b exist in base.
    ///   mv b a   (overwrite a with b)
    ///   mv a c   (move again)
    ///
    /// Expected: Renamed(b→c) + Deleted(a)
    #[test]
    fn rename_overwrite_base_then_move() {
        let dir = setup_test_dir();
        let mut data = Vec::new();
        // mv b a (overwrite, both in base): D(b) + P(a, /dir/b)
        data.extend_from_slice(b"D\0/dir\0b\n");
        data.extend_from_slice(b"P\0/dir\0a\0f\0/dir/b\n");
        // mv a c: D(a) + R(c, /dir/b) [kernel follows redirect chain]
        data.extend_from_slice(b"D\0/dir\0a\n");
        data.extend_from_slice(b"R\0/dir\0c\0f\0/dir/b\n");
        fs::write(dir.path().join("journal"), &data).unwrap();

        let changes = resolve(read(dir.path())).unwrap();
        let has_rename = changes.iter().any(|c| {
            matches!(c, Change::Renamed { from, to, .. }
                if from == "/dir/b" && to == "/dir/c")
        });
        let has_delete = changes
            .iter()
            .any(|c| matches!(c, Change::Deleted(p) if p == "/dir/a"));
        assert!(has_rename, "expected Renamed(b→c), got: {changes:?}");
        assert!(
            has_delete,
            "expected Deleted(a) for overwritten base file, got: {changes:?}"
        );
    }

    /// Same issue but simpler: rename overwrites base file, then delete.
    ///
    /// Scenario: both /dir/a and /dir/b exist in base.
    ///   mv b a   (overwrite a with b)
    ///   rm a     (delete)
    ///
    /// Expected: Deleted(a) + Deleted(b)
    #[test]
    fn rename_overwrite_base_then_delete() {
        let dir = setup_test_dir();
        let mut data = Vec::new();
        // mv b a (overwrite, both in base): D(b) + P(a, /dir/b)
        data.extend_from_slice(b"D\0/dir\0b\n");
        data.extend_from_slice(b"P\0/dir\0a\0f\0/dir/b\n");
        // rm a: D(a)
        data.extend_from_slice(b"D\0/dir\0a\n");
        fs::write(dir.path().join("journal"), &data).unwrap();

        let changes = resolve(read(dir.path())).unwrap();
        let has_delete_a = changes
            .iter()
            .any(|c| matches!(c, Change::Deleted(p) if p == "/dir/a"));
        let has_delete_b = changes
            .iter()
            .any(|c| matches!(c, Change::Deleted(p) if p == "/dir/b"));
        assert!(
            has_delete_a,
            "expected Deleted(a) for overwritten base file, got: {changes:?}"
        );
        assert!(
            has_delete_b,
            "expected Deleted(b) for renamed-away file, got: {changes:?}"
        );
    }

    // -- P tag (replace-redirect) tests --

    /// R (redirect to new path) + delete: only the source needs deletion.
    #[test]
    fn redirect_new_path_then_delete() {
        let dir = setup_test_dir();
        let mut data = Vec::new();
        // mv a b (b is new): D(a) + R(b, /a)
        data.extend_from_slice(b"D\0/dir\0a\n");
        data.extend_from_slice(b"R\0/dir\0b\0f\0/dir/a\n");
        // rm b: D(b)
        data.extend_from_slice(b"D\0/dir\0b\n");
        fs::write(dir.path().join("journal"), &data).unwrap();

        let changes = resolve(read(dir.path())).unwrap();
        assert_eq!(changes.len(), 1, "expected 1 change, got: {changes:?}");
        assert!(
            matches!(&changes[0], Change::Deleted(p) if p == "/dir/a"),
            "expected Deleted(a), got: {changes:?}"
        );
    }

    /// P (replace-redirect, destination in base) + delete: both paths deleted.
    #[test]
    fn replace_redirect_then_delete() {
        let dir = setup_test_dir();
        let mut data = Vec::new();
        // mv a b (b exists in base): D(a) + P(b, /a)
        data.extend_from_slice(b"D\0/dir\0a\n");
        data.extend_from_slice(b"P\0/dir\0b\0f\0/dir/a\n");
        // rm b: D(b)
        data.extend_from_slice(b"D\0/dir\0b\n");
        fs::write(dir.path().join("journal"), &data).unwrap();

        let changes = resolve(read(dir.path())).unwrap();
        let has_delete_a = changes
            .iter()
            .any(|c| matches!(c, Change::Deleted(p) if p == "/dir/a"));
        let has_delete_b = changes
            .iter()
            .any(|c| matches!(c, Change::Deleted(p) if p == "/dir/b"));
        assert!(has_delete_a, "expected Deleted(a), got: {changes:?}");
        assert!(has_delete_b, "expected Deleted(b), got: {changes:?}");
    }

    /// Simple P (replace-redirect): rename overwrites base file.
    #[test]
    fn replace_redirect_simple() {
        let dir = setup_test_dir();
        let mut data = Vec::new();
        // mv a b (b exists in base): D(a) + P(b, /a)
        data.extend_from_slice(b"D\0/dir\0a\n");
        data.extend_from_slice(b"P\0/dir\0b\0f\0/dir/a\n");
        fs::write(dir.path().join("journal"), &data).unwrap();

        let changes = resolve(read(dir.path())).unwrap();
        assert_eq!(changes.len(), 1, "expected 1 change, got: {changes:?}");
        assert!(
            matches!(&changes[0], Change::Replaced { from, to, .. }
                if from == "/dir/a" && to == "/dir/b"),
            "expected Replaced(a→b), got: {changes:?}"
        );
    }

    /// Redirect (R) overwrites prior staged add — staged content dropped.
    #[test]
    fn redirect_overwrites_prior_stage_new() {
        let dir = setup_test_dir();
        let mut data = Vec::new();
        // touch b (staged new): A(b, ino=1)
        data.extend_from_slice(b"A\0/dir\0b\0f\01\n");
        // mv a b (b not originally in base): D(a) + R(b, /a)
        data.extend_from_slice(b"D\0/dir\0a\n");
        data.extend_from_slice(b"R\0/dir\0b\0f\0/dir/a\n");
        fs::write(dir.path().join("journal"), &data).unwrap();
        fs::write(dir.path().join("inodes/1"), "staged").unwrap();

        let changes = resolve(read(dir.path())).unwrap();
        assert_eq!(changes.len(), 1, "expected 1 change, got: {changes:?}");
        assert!(
            matches!(&changes[0], Change::Renamed { from, to, .. }
                if from == "/dir/a" && to == "/dir/b"),
            "expected Renamed(a→b), staged add silently replaced, got: {changes:?}"
        );
    }

    /// P overwrites prior modify — staged content dropped, in_base preserved.
    #[test]
    fn replace_overwrites_prior_modify() {
        let dir = setup_test_dir();
        let mut data = Vec::new();
        // echo >> b (COW modify): M(b, ino=1)
        data.extend_from_slice(b"M\0/dir\0b\0f\01\n");
        // mv a b (b in base): D(a) + P(b, /a)
        data.extend_from_slice(b"D\0/dir\0a\n");
        data.extend_from_slice(b"P\0/dir\0b\0f\0/dir/a\n");
        fs::write(dir.path().join("journal"), &data).unwrap();
        fs::write(dir.path().join("inodes/1"), "modified").unwrap();

        let changes = resolve(read(dir.path())).unwrap();
        assert_eq!(changes.len(), 1, "expected 1 change, got: {changes:?}");
        assert!(
            matches!(&changes[0], Change::Replaced { from, to, .. }
                if from == "/dir/a" && to == "/dir/b"),
            "expected Replaced(a→b), prior modify dropped, got: {changes:?}"
        );
    }

    /// Chain rename through a base path: mv b a (overwrite) then mv a c.
    #[test]
    fn chain_rename_through_base_path() {
        let dir = setup_test_dir();
        let mut data = Vec::new();
        // mv b a (overwrite, both in base): D(b) + P(a, /b)
        data.extend_from_slice(b"D\0/dir\0b\n");
        data.extend_from_slice(b"P\0/dir\0a\0f\0/dir/b\n");
        // mv a c (c is new): D(a) + R(c, /b) [kernel follows chain]
        data.extend_from_slice(b"D\0/dir\0a\n");
        data.extend_from_slice(b"R\0/dir\0c\0f\0/dir/b\n");
        fs::write(dir.path().join("journal"), &data).unwrap();

        let changes = resolve(read(dir.path())).unwrap();
        let has_rename = changes.iter().any(|c| {
            matches!(c, Change::Renamed { from, to, .. }
                if from == "/dir/b" && to == "/dir/c")
        });
        let has_delete = changes
            .iter()
            .any(|c| matches!(c, Change::Deleted(p) if p == "/dir/a"));
        assert!(has_rename, "expected Renamed(b→c), got: {changes:?}");
        assert!(
            has_delete,
            "expected Deleted(a) for overwritten base file, got: {changes:?}"
        );
    }

    // -- P+P (consecutive overwrites) tests --

    /// Two consecutive replace-redirects to the same destination.
    ///
    /// Scenario: /dir/a, /dir/b, /dir/c all exist in base.
    ///   mv a b   (overwrite b with a)
    ///   mv c b   (overwrite b again with c)
    ///
    /// Expected: Deleted(a) + Replaced(c→b)
    #[test]
    fn consecutive_replace_redirects() {
        let dir = setup_test_dir();
        let mut data = Vec::new();
        // mv a b (overwrite, both in base): D(a) + P(b, /dir/a)
        data.extend_from_slice(b"D\0/dir\0a\n");
        data.extend_from_slice(b"P\0/dir\0b\0f\0/dir/a\n");
        // mv c b (overwrite again): D(c) + P(b, /dir/c)
        data.extend_from_slice(b"D\0/dir\0c\n");
        data.extend_from_slice(b"P\0/dir\0b\0f\0/dir/c\n");
        fs::write(dir.path().join("journal"), &data).unwrap();

        let changes = resolve(read(dir.path())).unwrap();
        let has_delete_a = changes
            .iter()
            .any(|c| matches!(c, Change::Deleted(p) if p == "/dir/a"));
        let has_replaced = changes.iter().any(|c| {
            matches!(c, Change::Replaced { from, to, .. }
                if from == "/dir/c" && to == "/dir/b")
        });
        assert!(has_delete_a, "expected Deleted(a) from first overwrite, got: {changes:?}");
        assert!(has_replaced, "expected Replaced(c→b), got: {changes:?}");
    }

    // -- Checkpoint tests --

    /// Helper: slice at a checkpoint and resolve.
    fn resolve_at(records: Vec<Record>, name: &str) -> Result<Vec<Change>> {
        resolve(slice_records(records, Some(name), None, None)?)
    }

    #[test]
    fn resolve_at_checkpoint() {
        let dir = setup_test_dir();
        let mut data = Vec::new();
        data.extend_from_slice(b"A\0/nonexistent_test_12345\0x\0f\01\n");
        data.extend_from_slice(b"K\01\0chk1\n");
        data.extend_from_slice(b"A\0/nonexistent_test_12345\0x\0f\02\n");
        data.extend_from_slice(b"A\0/nonexistent_test_12345\0y\0f\03\n");
        fs::write(dir.path().join("journal"), &data).unwrap();
        fs::write(dir.path().join("inodes/1"), "v1").unwrap();
        fs::write(dir.path().join("inodes/2"), "v2").unwrap();
        fs::write(dir.path().join("inodes/3"), "v3").unwrap();

        let changes = resolve_at(read(dir.path()), "chk1").unwrap();
        assert_eq!(changes.len(), 1, "at chk1: {changes:?}");
        assert!(matches!(&changes[0], Change::Added { path, ino, .. }
            if path == "/nonexistent_test_12345/x" && *ino == 1));

        let all = resolve(read(dir.path())).unwrap();
        assert_eq!(all.len(), 2, "full: {all:?}");
    }

    #[test]
    fn resolve_at_matches_latest_checkpoint() {
        let dir = setup_test_dir();
        let mut data = Vec::new();
        data.extend_from_slice(b"A\0/nonexistent_test_12345\0x\0f\01\n");
        data.extend_from_slice(b"K\01\0dup\n");
        data.extend_from_slice(b"A\0/nonexistent_test_12345\0y\0f\02\n");
        data.extend_from_slice(b"K\02\0dup\n");
        data.extend_from_slice(b"A\0/nonexistent_test_12345\0z\0f\03\n");
        fs::write(dir.path().join("journal"), &data).unwrap();
        fs::write(dir.path().join("inodes/1"), "").unwrap();
        fs::write(dir.path().join("inodes/2"), "").unwrap();
        fs::write(dir.path().join("inodes/3"), "").unwrap();

        let changes = resolve_at(read(dir.path()), "dup").unwrap();
        // Latest "dup" is the second checkpoint; its segment contains only A(y).
        assert_eq!(changes.len(), 1, "at latest dup: {changes:?}");
        assert!(matches!(&changes[0], Change::Added { path, ino, .. }
            if path == "/nonexistent_test_12345/y" && *ino == 2));
    }

    #[test]
    fn resolve_at_not_found() {
        let dir = setup_test_dir();
        fs::write(dir.path().join("journal"), b"A\0\0a\0f\01\n").unwrap();
        assert!(resolve_at(read(dir.path()), "nonexistent").is_err());
    }

    #[test]
    fn resolve_at_by_numeric_id() {
        let dir = setup_test_dir();
        let mut data = Vec::new();
        data.extend_from_slice(b"A\0/nonexistent_test_12345\0x\0f\01\n");
        data.extend_from_slice(b"K\05\0mychk\n");
        data.extend_from_slice(b"A\0/nonexistent_test_12345\0y\0f\02\n");
        fs::write(dir.path().join("journal"), &data).unwrap();
        fs::write(dir.path().join("inodes/1"), "").unwrap();
        fs::write(dir.path().join("inodes/2"), "").unwrap();

        let changes = resolve_at(read(dir.path()), "5").unwrap();
        assert_eq!(changes.len(), 1, "by id: {changes:?}");

        let changes2 = resolve_at(read(dir.path()), "mychk").unwrap();
        assert_eq!(changes2.len(), 1, "by name: {changes2:?}");
    }

    #[test]
    fn resolve_at_id_not_found() {
        let dir = setup_test_dir();
        let mut data = Vec::new();
        data.extend_from_slice(b"K\01\0chk\n");
        fs::write(dir.path().join("journal"), &data).unwrap();
        assert!(resolve_at(read(dir.path()), "99").is_err());
    }

    #[test]
    fn resolve_at_id_takes_priority_over_name() {
        let dir = setup_test_dir();
        let mut data = Vec::new();
        data.extend_from_slice(b"A\0/nonexistent_test_12345\0x\0f\01\n");
        data.extend_from_slice(b"K\01\0first\n");
        data.extend_from_slice(b"A\0/nonexistent_test_12345\0y\0f\02\n");
        data.extend_from_slice(b"K\02\01\n");
        fs::write(dir.path().join("journal"), &data).unwrap();
        fs::write(dir.path().join("inodes/1"), "").unwrap();
        fs::write(dir.path().join("inodes/2"), "").unwrap();

        let changes = resolve_at(read(dir.path()), "1").unwrap();
        assert_eq!(
            changes.len(),
            1,
            "id=1 should find first checkpoint: {changes:?}"
        );

        // id=2 is the second checkpoint; its segment contains only A(y).
        let changes2 = resolve_at(read(dir.path()), "2").unwrap();
        assert_eq!(
            changes2.len(),
            1,
            "id=2 should find second checkpoint segment: {changes2:?}"
        );

        let changes3 = resolve_at(read(dir.path()), "first").unwrap();
        assert_eq!(changes3.len(), 1, "name=first: {changes3:?}");
    }

    #[test]
    fn resolver_single_pass_checkpoint() {
        let dir = setup_test_dir();
        let mut data = Vec::new();
        data.extend_from_slice(b"A\0/nonexistent_test_12345\0x\0f\01\n");
        data.extend_from_slice(b"K\03\0chk\n");
        data.extend_from_slice(b"A\0/nonexistent_test_12345\0y\0f\02\n");
        fs::write(dir.path().join("journal"), &data).unwrap();
        fs::write(dir.path().join("inodes/1"), "").unwrap();
        fs::write(dir.path().join("inodes/2"), "").unwrap();

        let records = read(dir.path());
        let chk_idx = find_checkpoint_index(&records, "3").unwrap();
        let mut resolver = Resolver::new();
        let mut iter = records.into_iter();
        for record in iter.by_ref().take(chk_idx + 1) {
            resolver.process(record);
        }
        let at_chk = resolver.clone().into_changes();
        for record in iter {
            resolver.process(record);
        }
        let current = resolver.into_changes();

        assert_eq!(at_chk.len(), 1, "at chk: {at_chk:?}");
        assert_eq!(current.len(), 2, "current: {current:?}");
    }

    #[test]
    fn resolve_skips_checkpoint_records() {
        let dir = setup_test_dir();
        let mut data = Vec::new();
        data.extend_from_slice(b"A\0/nonexistent_test_12345\0x\0f\01\n");
        data.extend_from_slice(b"K\01\0chk\n");
        fs::write(dir.path().join("journal"), &data).unwrap();
        fs::write(dir.path().join("inodes/1"), "content").unwrap();

        let changes = resolve(read(dir.path())).unwrap();
        assert_eq!(changes.len(), 1);
        assert!(matches!(&changes[0], Change::Added { .. }));
    }

    // -- resolve_segments tests --

    #[test]
    fn segments_no_checkpoints() {
        let dir = setup_test_dir();
        let mut data = Vec::new();
        data.extend_from_slice(b"A\0/nonexistent_test_12345\0a\0f\01\n");
        data.extend_from_slice(b"A\0/nonexistent_test_12345\0b\0f\02\n");
        fs::write(dir.path().join("journal"), &data).unwrap();
        fs::write(dir.path().join("inodes/1"), "").unwrap();
        fs::write(dir.path().join("inodes/2"), "").unwrap();

        let segments = resolve_segments(read(dir.path())).unwrap();
        assert!(segments.is_empty());
    }

    #[test]
    fn segments_one_checkpoint_no_trailing() {
        let dir = setup_test_dir();
        let mut data = Vec::new();
        data.extend_from_slice(b"K\01\0build\n");
        fs::write(dir.path().join("journal"), &data).unwrap();

        let segments = resolve_segments(read(dir.path())).unwrap();
        assert!(segments.is_empty());
    }

    #[test]
    fn segments_two_checkpoints_with_trailing() {
        let dir = setup_test_dir();
        let mut data = Vec::new();
        data.extend_from_slice(b"K\01\0first\n");
        data.extend_from_slice(b"A\0/nonexistent_test_12345\0b\0f\02\n");
        data.extend_from_slice(b"K\02\0second\n");
        data.extend_from_slice(b"A\0/nonexistent_test_12345\0c\0f\03\n");
        fs::write(dir.path().join("journal"), &data).unwrap();
        fs::write(dir.path().join("inodes/2"), "").unwrap();
        fs::write(dir.path().join("inodes/3"), "").unwrap();

        let segments = resolve_segments(read(dir.path())).unwrap();
        assert_eq!(segments.len(), 2, "{segments:?}");
        assert_eq!(segments[0].from, Checkpoint { gen_id: 1, name: "first".into() });
        assert_eq!(segments[0].to, Some(Checkpoint { gen_id: 2, name: "second".into() }));
        assert_eq!(segments[0].changes.len(), 1, "first→second: {:?}", segments[0].changes);
        assert_eq!(segments[1].from, Checkpoint { gen_id: 2, name: "second".into() });
        assert!(segments[1].to.is_none());
        assert_eq!(segments[1].changes.len(), 1, "trailing: {:?}", segments[1].changes);
    }

    #[test]
    fn segments_modify_across_checkpoints() {
        let dir = setup_test_dir();
        let mut data = Vec::new();
        data.extend_from_slice(b"A\0/nonexistent_test_12345\0x\0f\01\n");
        data.extend_from_slice(b"K\01\0chk1\n");
        // Re-COW after checkpoint: kernel emits M (file exists after chk1)
        data.extend_from_slice(b"M\0/nonexistent_test_12345\0x\0f\02\n");
        data.extend_from_slice(b"K\02\0chk2\n");
        fs::write(dir.path().join("journal"), &data).unwrap();
        fs::write(dir.path().join("inodes/1"), "v1").unwrap();
        fs::write(dir.path().join("inodes/2"), "v2").unwrap();

        let segments = resolve_segments(read(dir.path())).unwrap();
        assert_eq!(segments.len(), 1, "{segments:?}");

        assert_eq!(segments[0].from, Checkpoint { gen_id: 1, name: "chk1".into() });
        assert_eq!(segments[0].to, Some(Checkpoint { gen_id: 2, name: "chk2".into() }));
        assert_eq!(segments[0].changes.len(), 1);
        assert!(
            matches!(&segments[0].changes[0], Change::Modified { path, ino, .. }
            if path == "/nonexistent_test_12345/x" && *ino == 2)
        );
    }

    /// Base file modified in segment 1, re-COW in segment 2: both are Modified.
    #[test]
    fn segments_base_modify_across_checkpoints() {
        let dir = setup_test_dir();
        let mut data = Vec::new();
        data.extend_from_slice(b"M\0/etc\0hostname\0f\01\n");
        data.extend_from_slice(b"K\01\0chk1\n");
        data.extend_from_slice(b"M\0/etc\0hostname\0f\02\n");
        data.extend_from_slice(b"K\02\0chk2\n");
        fs::write(dir.path().join("journal"), &data).unwrap();
        fs::write(dir.path().join("inodes/1"), "v1").unwrap();
        fs::write(dir.path().join("inodes/2"), "v2").unwrap();

        let segments = resolve_segments(read(dir.path())).unwrap();
        assert_eq!(segments.len(), 1, "{segments:?}");

        assert_eq!(segments[0].changes.len(), 1);
        assert!(
            matches!(&segments[0].changes[0], Change::Modified { path, ino, .. }
            if path == "/etc/hostname" && *ino == 2),
            "seg: expected Modified(hostname, 2), got: {:?}",
            segments[0].changes
        );
    }

    #[test]
    fn segments_empty_checkpoint() {
        let dir = setup_test_dir();
        let mut data = Vec::new();
        data.extend_from_slice(b"K\01\0empty\n");
        data.extend_from_slice(b"A\0/nonexistent_test_12345\0a\0f\01\n");
        fs::write(dir.path().join("journal"), &data).unwrap();
        fs::write(dir.path().join("inodes/1"), "").unwrap();

        let segments = resolve_segments(read(dir.path())).unwrap();
        assert_eq!(segments.len(), 1);
        assert_eq!(segments[0].from, Checkpoint { gen_id: 1, name: "empty".into() });
        assert!(segments[0].to.is_none());
        assert_eq!(segments[0].changes.len(), 1);
    }

    #[test]
    fn segments_delete_in_later_segment() {
        let dir = setup_test_dir();
        let mut data = Vec::new();
        data.extend_from_slice(b"A\0/nonexistent_test_12345\0x\0f\01\n");
        data.extend_from_slice(b"K\01\0chk1\n");
        data.extend_from_slice(b"D\0/nonexistent_test_12345\0x\n");
        data.extend_from_slice(b"K\02\0chk2\n");
        fs::write(dir.path().join("journal"), &data).unwrap();
        fs::write(dir.path().join("inodes/1"), "content").unwrap();

        let segments = resolve_segments(read(dir.path())).unwrap();
        assert_eq!(segments.len(), 1, "{segments:?}");
    }

    #[test]
    fn segments_rename_in_later_segment() {
        let dir = setup_test_dir();
        let mut data = Vec::new();
        data.extend_from_slice(b"A\0/nonexistent_test_12345\0a\0f\01\n");
        data.extend_from_slice(b"K\01\0chk1\n");
        // Staged rename: delete old + staged new (same ino)
        data.extend_from_slice(b"D\0/nonexistent_test_12345\0a\n");
        data.extend_from_slice(b"A\0/nonexistent_test_12345\0b\0f\01\n");
        data.extend_from_slice(b"K\02\0chk2\n");
        fs::write(dir.path().join("journal"), &data).unwrap();
        fs::write(dir.path().join("inodes/1"), "content").unwrap();

        let segments = resolve_segments(read(dir.path())).unwrap();
        assert_eq!(segments.len(), 1, "{segments:?}");

        let chk2 = &segments[0].changes;
        assert!(!chk2.is_empty(), "chk2 should have changes: {chk2:?}");
        let has_b = chk2.iter().any(|c| {
            matches!(c, Change::Added { path, ino, .. }
                if path == "/nonexistent_test_12345/b" && *ino == 1)
        });
        assert!(has_b, "expected /b added in chk2, got: {chk2:?}");
    }

    /// Redirect-rename appearing in a later segment must emit Renamed in delta.
    /// Segment 1: stage file at /b. Segment 2: rename /c → /b (redirect).
    /// The delta for segment 2 should contain a Renamed change.
    #[test]
    fn segments_redirect_rename_in_later_segment() {
        let dir = setup_test_dir();
        let mut data = Vec::new();
        // Segment 1: stage a file at /b
        data.extend_from_slice(b"A\0/nonexistent_test_12345\0b\0f\01\n");
        data.extend_from_slice(b"K\01\0chk1\n");
        // Segment 2: redirect-rename /c → /b
        // Kernel emits: E(c, Deleted) + E(b, Redirect(c))
        data.extend_from_slice(b"D\0/nonexistent_test_12345\0c\n");
        data.extend_from_slice(b"R\0/nonexistent_test_12345\0b\0f\0/nonexistent_test_12345/c\n");
        data.extend_from_slice(b"K\02\0chk2\n");
        fs::write(dir.path().join("journal"), &data).unwrap();
        fs::write(dir.path().join("inodes/1"), "content").unwrap();

        let segments = resolve_segments(read(dir.path())).unwrap();
        assert_eq!(segments.len(), 1, "{segments:?}");

        // Segment should contain the rename
        let chk2 = &segments[0].changes;
        let has_rename = chk2.iter().any(|c| {
            matches!(c, Change::Renamed { from, to, .. }
                if from == "/nonexistent_test_12345/c" && to == "/nonexistent_test_12345/b")
        });
        assert!(has_rename, "expected Renamed(c→b) in chk2, got: {chk2:?}");
    }

    #[test]
    fn segments_multiple_files_per_segment() {
        let dir = setup_test_dir();
        let mut data = Vec::new();
        data.extend_from_slice(b"A\0/nonexistent_test_12345\0a\0f\01\n");
        data.extend_from_slice(b"A\0/nonexistent_test_12345\0b\0f\02\n");
        data.extend_from_slice(b"A\0/nonexistent_test_12345\0c\0f\03\n");
        data.extend_from_slice(b"K\01\0chk1\n");
        data.extend_from_slice(b"A\0/nonexistent_test_12345\0d\0f\04\n");
        data.extend_from_slice(b"A\0/nonexistent_test_12345\0e\0f\05\n");
        data.extend_from_slice(b"K\02\0chk2\n");
        fs::write(dir.path().join("journal"), &data).unwrap();
        for i in 1..=5 {
            fs::write(dir.path().join(format!("inodes/{i}")), "").unwrap();
        }

        let segments = resolve_segments(read(dir.path())).unwrap();
        assert_eq!(segments.len(), 1, "{segments:?}");
        assert_eq!(
            segments[0].changes.len(),
            2,
            "chk1→chk2: {:?}",
            segments[0].changes
        );
    }

    #[test]
    fn segments_empty_journal() {
        let dir = setup_test_dir();
        let segments = resolve_segments(read(dir.path())).unwrap();
        assert!(segments.is_empty());
    }

    #[test]
    fn segments_only_checkpoint_records() {
        let dir = setup_test_dir();
        let mut data = Vec::new();
        data.extend_from_slice(b"K\01\0chk1\n");
        data.extend_from_slice(b"K\02\0chk2\n");
        fs::write(dir.path().join("journal"), &data).unwrap();

        let segments = resolve_segments(read(dir.path())).unwrap();
        assert_eq!(segments.len(), 1, "{segments:?}");
        assert!(segments[0].changes.is_empty());
    }

    #[test]
    fn segments_three_checkpoints() {
        let dir = setup_test_dir();
        let mut data = Vec::new();
        data.extend_from_slice(b"K\01\0s1\n");
        data.extend_from_slice(b"A\0/nonexistent_test_12345\0b\0f\02\n");
        data.extend_from_slice(b"K\02\0s2\n");
        data.extend_from_slice(b"A\0/nonexistent_test_12345\0c\0f\03\n");
        data.extend_from_slice(b"K\03\0s3\n");
        fs::write(dir.path().join("journal"), &data).unwrap();
        for i in 2..=3 {
            fs::write(dir.path().join(format!("inodes/{i}")), "").unwrap();
        }

        let segments = resolve_segments(read(dir.path())).unwrap();
        assert_eq!(segments.len(), 2, "{segments:?}");
        assert_eq!(segments[0].from, Checkpoint { gen_id: 1, name: "s1".into() });
        assert_eq!(segments[0].to, Some(Checkpoint { gen_id: 2, name: "s2".into() }));
        assert_eq!(segments[0].changes.len(), 1, "s1→s2: {:?}", segments[0].changes);
        assert_eq!(segments[1].from, Checkpoint { gen_id: 2, name: "s2".into() });
        assert_eq!(segments[1].to, Some(Checkpoint { gen_id: 3, name: "s3".into() }));
        assert_eq!(segments[1].changes.len(), 1, "s2→s3: {:?}", segments[1].changes);
    }

    /// Delete + re-create within the same segment must show Modified (not Added)
    /// when the file existed in the previous checkpoint.
    /// The kernel emits D + A (not M) because the re-create goes through VFS create.
    #[test]
    fn segments_delete_recreate_same_path_across_checkpoints() {
        let dir = setup_test_dir();
        let mut data = Vec::new();
        // Segment 1: create /x
        data.extend_from_slice(b"A\0/nonexistent_test_12345\0x\0f\01\n");
        data.extend_from_slice(b"K\01\0chk1\n");
        // Segment 2: delete /x then re-create /x.
        // Kernel emits D + M (modify, because /x exists in base).
        // Resolver: D inserts Delete, M replaces it with Stage(is_new=false) → Modified.
        data.extend_from_slice(b"D\0/nonexistent_test_12345\0x\n");
        data.extend_from_slice(b"M\0/nonexistent_test_12345\0x\0f\02\n");
        data.extend_from_slice(b"K\02\0chk2\n");
        fs::write(dir.path().join("journal"), &data).unwrap();
        fs::write(dir.path().join("inodes/1"), "v1").unwrap();
        fs::write(dir.path().join("inodes/2"), "v2").unwrap();

        let segments = resolve_segments(read(dir.path())).unwrap();
        assert_eq!(segments.len(), 1, "{segments:?}");

        // Segment (K1→K2): Modified (file existed in base from chk1)
        assert_eq!(
            segments[0].changes.len(),
            1,
            "chk2: {:?}",
            segments[0].changes
        );
        assert!(
            matches!(
                &segments[0].changes[0],
                Change::Modified { path, ino, .. }
                if path == "/nonexistent_test_12345/x" && *ino == 2
            ),
            "expected Modified in chk2, got: {:?}",
            segments[0].changes[0]
        );
    }

    #[test]
    fn segments_add_delete_readd_across_checkpoints() {
        let dir = setup_test_dir();
        let mut data = Vec::new();
        data.extend_from_slice(b"A\0/nonexistent_test_12345\0x\0f\01\n");
        data.extend_from_slice(b"K\01\0chk1\n");
        data.extend_from_slice(b"D\0/nonexistent_test_12345\0x\n");
        data.extend_from_slice(b"K\02\0chk2\n");
        data.extend_from_slice(b"A\0/nonexistent_test_12345\0x\0f\02\n");
        fs::write(dir.path().join("journal"), &data).unwrap();
        fs::write(dir.path().join("inodes/1"), "v1").unwrap();
        fs::write(dir.path().join("inodes/2"), "v2").unwrap();

        let segments = resolve_segments(read(dir.path())).unwrap();
        assert_eq!(segments.len(), 2, "{segments:?}");

        // Segment 0 (K1→K2): Deleted
        let has_delete = segments[0].changes.iter().any(|c| {
            matches!(c, Change::Deleted(path) if path == "/nonexistent_test_12345/x")
        });
        assert!(has_delete, "expected Deleted in K1→K2, got: {:?}", segments[0].changes);

        // Segment 1 (K2→None): re-Added
        let has_x = segments[1].changes.iter().any(|c| {
            matches!(c, Change::Added { path, ino, .. }
                if path == "/nonexistent_test_12345/x" && *ino == 2)
        });
        assert!(
            has_x,
            "expected re-add in trailing, got: {:?}",
            segments[1].changes
        );
    }

    #[test]
    fn segments_rename_modified_in_segment() {
        let dir = setup_test_dir();
        let mut data = Vec::new();
        data.extend_from_slice(b"A\0/nonexistent_test_12345\0a\0f\01\n");
        data.extend_from_slice(b"K\01\0chk1\n");
        // Staged rename a->b then modify b (new ino)
        data.extend_from_slice(b"D\0/nonexistent_test_12345\0a\n");
        data.extend_from_slice(b"A\0/nonexistent_test_12345\0b\0f\01\n");
        data.extend_from_slice(b"A\0/nonexistent_test_12345\0b\0f\02\n");
        data.extend_from_slice(b"K\02\0chk2\n");
        fs::write(dir.path().join("journal"), &data).unwrap();
        fs::write(dir.path().join("inodes/1"), "v1").unwrap();
        fs::write(dir.path().join("inodes/2"), "v2").unwrap();

        let segments = resolve_segments(read(dir.path())).unwrap();
        assert_eq!(segments.len(), 1, "{segments:?}");

        let chk2 = &segments[0].changes;
        assert!(!chk2.is_empty(), "chk2 should have changes: {chk2:?}");
    }

    /// Segments must preserve dtype — directories should not become DType::File.
    #[test]
    fn segments_preserve_dtype_for_directories() {
        let dir = setup_test_dir();
        let mut data = Vec::new();
        // Create a directory in chk1
        data.extend_from_slice(b"A\0/nonexistent_test_12345\0mydir\0d\01\n");
        data.extend_from_slice(b"K\01\0chk1\n");
        // Create a file in chk2
        data.extend_from_slice(b"A\0/nonexistent_test_12345\0file\0f\02\n");
        data.extend_from_slice(b"K\02\0chk2\n");
        fs::write(dir.path().join("journal"), &data).unwrap();
        fs::create_dir_all(dir.path().join("inodes/1")).unwrap();
        fs::write(dir.path().join("inodes/2"), "").unwrap();

        let segments = resolve_segments(read(dir.path())).unwrap();
        assert_eq!(segments.len(), 1, "{segments:?}");

        // chk1→chk2: file added — dtype must be File
        assert_eq!(segments[0].changes.len(), 1);
        assert!(
            matches!(&segments[0].changes[0], Change::Added { path, dtype: DType::File, .. }
            if path == "/nonexistent_test_12345/file"),
            "expected Added with DType::File, got: {:?}",
            segments[0].changes[0]
        );
    }

    /// Segments must preserve dtype through rename deltas.
    #[test]
    fn segments_preserve_dtype_for_renamed_symlink() {
        let dir = setup_test_dir();
        let mut data = Vec::new();
        // Create a symlink in chk1
        data.extend_from_slice(b"A\0/nonexistent_test_12345\0link\0l\01\n");
        data.extend_from_slice(b"K\01\0chk1\n");
        // Rename link -> link2 (staged rename: delete + staged with same ino)
        data.extend_from_slice(b"D\0/nonexistent_test_12345\0link\n");
        data.extend_from_slice(b"A\0/nonexistent_test_12345\0link2\0l\01\n");
        data.extend_from_slice(b"K\02\0chk2\n");
        fs::write(dir.path().join("journal"), &data).unwrap();
        std::os::unix::fs::symlink("target", dir.path().join("inodes/1")).unwrap();

        let segments = resolve_segments(read(dir.path())).unwrap();
        assert_eq!(segments.len(), 1, "{segments:?}");

        // chk1→chk2: the delta should show link2 as Added with DType::Link
        let chk2 = &segments[0].changes;
        let link2 = chk2
            .iter()
            .find(|c| matches!(c, Change::Added { path, .. } if path.ends_with("/link2")));
        assert!(link2.is_some(), "expected link2 in chk2: {chk2:?}");
        assert!(
            matches!(
                link2.unwrap(),
                Change::Added {
                    dtype: DType::Link,
                    ..
                }
            ),
            "expected DType::Link, got: {:?}",
            link2.unwrap()
        );
    }

    /// Segments must preserve dtype when a modify crosses checkpoints.
    #[test]
    fn segments_preserve_dtype_for_modified() {
        let dir = setup_test_dir();
        let mut data = Vec::new();
        data.extend_from_slice(b"A\0/nonexistent_test_12345\0mydir\0d\01\n");
        data.extend_from_slice(b"K\01\0chk1\n");
        // Re-stage directory with new ino (modify, since it was added in chk1)
        data.extend_from_slice(b"M\0/nonexistent_test_12345\0mydir\0d\02\n");
        data.extend_from_slice(b"K\02\0chk2\n");
        fs::write(dir.path().join("journal"), &data).unwrap();
        fs::create_dir_all(dir.path().join("inodes/1")).unwrap();
        fs::create_dir_all(dir.path().join("inodes/2")).unwrap();

        let segments = resolve_segments(read(dir.path())).unwrap();
        assert_eq!(segments.len(), 1, "{segments:?}");

        // chk1→chk2: modified directory — dtype must be Dir
        assert_eq!(segments[0].changes.len(), 1);
        assert!(
            matches!(
                &segments[0].changes[0],
                Change::Modified {
                    dtype: DType::Dir,
                    ..
                }
            ),
            "expected Modified with DType::Dir, got: {:?}",
            segments[0].changes[0]
        );
    }

    // -- slice_records tests --

    #[test]
    fn slice_none_returns_all() {
        let dir = setup_test_dir();
        let mut data = Vec::new();
        data.extend_from_slice(b"A\0/nonexistent_test_12345\0a\0f\01\n");
        data.extend_from_slice(b"K\01\0chk1\n");
        data.extend_from_slice(b"A\0/nonexistent_test_12345\0b\0f\02\n");
        fs::write(dir.path().join("journal"), &data).unwrap();

        let records = read(dir.path());
        let n = records.len();
        let sliced = slice_records(records, None, None, None).unwrap();
        assert_eq!(sliced.len(), n);
    }

    #[test]
    fn slice_at_isolates_segment() {
        let dir = setup_test_dir();
        let mut data = Vec::new();
        data.extend_from_slice(b"A\0/nonexistent_test_12345\0a\0f\01\n");
        data.extend_from_slice(b"K\01\0chk1\n");
        data.extend_from_slice(b"A\0/nonexistent_test_12345\0b\0f\02\n");
        data.extend_from_slice(b"K\02\0chk2\n");
        data.extend_from_slice(b"A\0/nonexistent_test_12345\0c\0f\03\n");
        fs::write(dir.path().join("journal"), &data).unwrap();

        // --at chk2 should give only the records between chk1 and chk2
        let sliced = slice_records(read(dir.path()), Some("chk2"), None, None).unwrap();
        let changes = resolve(sliced).unwrap();
        assert_eq!(changes.len(), 1, "{changes:?}");
        assert!(matches!(&changes[0], Change::Added { path, .. }
            if path == "/nonexistent_test_12345/b"));
    }

    #[test]
    fn slice_at_first_checkpoint() {
        let dir = setup_test_dir();
        let mut data = Vec::new();
        data.extend_from_slice(b"A\0/nonexistent_test_12345\0a\0f\01\n");
        data.extend_from_slice(b"K\01\0chk1\n");
        data.extend_from_slice(b"A\0/nonexistent_test_12345\0b\0f\02\n");
        fs::write(dir.path().join("journal"), &data).unwrap();

        // --at chk1: no previous checkpoint, so includes everything up to chk1
        let sliced = slice_records(read(dir.path()), Some("chk1"), None, None).unwrap();
        let changes = resolve(sliced).unwrap();
        assert_eq!(changes.len(), 1, "{changes:?}");
        assert!(matches!(&changes[0], Change::Added { path, .. }
            if path == "/nonexistent_test_12345/a"));
    }

    #[test]
    fn slice_from_only() {
        let dir = setup_test_dir();
        let mut data = Vec::new();
        data.extend_from_slice(b"A\0/nonexistent_test_12345\0a\0f\01\n");
        data.extend_from_slice(b"K\01\0chk1\n");
        data.extend_from_slice(b"A\0/nonexistent_test_12345\0b\0f\02\n");
        data.extend_from_slice(b"K\02\0chk2\n");
        data.extend_from_slice(b"A\0/nonexistent_test_12345\0c\0f\03\n");
        fs::write(dir.path().join("journal"), &data).unwrap();

        // --from chk1: everything after chk1
        let sliced = slice_records(read(dir.path()), None, Some("chk1"), None).unwrap();
        let changes = resolve(sliced).unwrap();
        assert_eq!(changes.len(), 2, "{changes:?}");
    }

    #[test]
    fn slice_to_only() {
        let dir = setup_test_dir();
        let mut data = Vec::new();
        data.extend_from_slice(b"A\0/nonexistent_test_12345\0a\0f\01\n");
        data.extend_from_slice(b"K\01\0chk1\n");
        data.extend_from_slice(b"A\0/nonexistent_test_12345\0b\0f\02\n");
        data.extend_from_slice(b"K\02\0chk2\n");
        data.extend_from_slice(b"A\0/nonexistent_test_12345\0c\0f\03\n");
        fs::write(dir.path().join("journal"), &data).unwrap();

        // --to chk1: everything up to and including chk1
        let sliced = slice_records(read(dir.path()), None, None, Some("chk1")).unwrap();
        let changes = resolve(sliced).unwrap();
        assert_eq!(changes.len(), 1, "{changes:?}");
        assert!(matches!(&changes[0], Change::Added { path, .. }
            if path == "/nonexistent_test_12345/a"));
    }

    #[test]
    fn slice_from_to_range() {
        let dir = setup_test_dir();
        let mut data = Vec::new();
        data.extend_from_slice(b"A\0/nonexistent_test_12345\0a\0f\01\n");
        data.extend_from_slice(b"K\01\0chk1\n");
        data.extend_from_slice(b"A\0/nonexistent_test_12345\0b\0f\02\n");
        data.extend_from_slice(b"K\02\0chk2\n");
        data.extend_from_slice(b"A\0/nonexistent_test_12345\0c\0f\03\n");
        data.extend_from_slice(b"K\03\0chk3\n");
        data.extend_from_slice(b"A\0/nonexistent_test_12345\0d\0f\04\n");
        fs::write(dir.path().join("journal"), &data).unwrap();

        // --from chk1 --to chk3: records after chk1 up to chk3
        let sliced = slice_records(read(dir.path()), None, Some("chk1"), Some("chk3")).unwrap();
        let changes = resolve(sliced).unwrap();
        assert_eq!(changes.len(), 2, "{changes:?}");
        // Should have b and c, not a or d
        assert!(
            changes
                .iter()
                .any(|c| matches!(c, Change::Added { path, .. }
            if path == "/nonexistent_test_12345/b"))
        );
        assert!(
            changes
                .iter()
                .any(|c| matches!(c, Change::Added { path, .. }
            if path == "/nonexistent_test_12345/c"))
        );
    }

    #[test]
    fn slice_from_to_preserves_segments() {
        let dir = setup_test_dir();
        let mut data = Vec::new();
        data.extend_from_slice(b"A\0/nonexistent_test_12345\0a\0f\01\n");
        data.extend_from_slice(b"K\01\0chk1\n");
        data.extend_from_slice(b"A\0/nonexistent_test_12345\0b\0f\02\n");
        data.extend_from_slice(b"K\02\0chk2\n");
        data.extend_from_slice(b"A\0/nonexistent_test_12345\0c\0f\03\n");
        data.extend_from_slice(b"K\03\0chk3\n");
        fs::write(dir.path().join("journal"), &data).unwrap();

        // --from chk1 --to chk3: should produce 2 segments (chk1→chk2, chk2→chk3)
        let sliced = slice_records(read(dir.path()), None, Some("chk1"), Some("chk3")).unwrap();
        let segments = resolve_segments(sliced).unwrap();
        assert_eq!(segments.len(), 2, "{segments:?}");
        assert_eq!(segments[0].to, Some(Checkpoint { gen_id: 2, name: "chk2".into() }));
        assert_eq!(segments[1].to, Some(Checkpoint { gen_id: 3, name: "chk3".into() }));
    }

    #[test]
    fn slice_from_to_same_checkpoint() {
        let dir = setup_test_dir();
        let mut data = Vec::new();
        data.extend_from_slice(b"A\0/nonexistent_test_12345\0a\0f\01\n");
        data.extend_from_slice(b"K\01\0chk1\n");
        data.extend_from_slice(b"A\0/nonexistent_test_12345\0b\0f\02\n");
        data.extend_from_slice(b"K\02\0chk2\n");
        data.extend_from_slice(b"A\0/nonexistent_test_12345\0c\0f\03\n");
        fs::write(dir.path().join("journal"), &data).unwrap();

        // --from chk1 --to chk1: truncate at chk1+1 then split_off at chk1+1 → empty
        let sliced = slice_records(read(dir.path()), None, Some("chk1"), Some("chk1")).unwrap();
        let changes = resolve(sliced).unwrap();
        assert!(
            changes.is_empty(),
            "same from/to should be empty: {changes:?}"
        );
    }

    #[test]
    fn slice_from_last_checkpoint() {
        let dir = setup_test_dir();
        let mut data = Vec::new();
        data.extend_from_slice(b"A\0/nonexistent_test_12345\0a\0f\01\n");
        data.extend_from_slice(b"K\01\0chk1\n");
        fs::write(dir.path().join("journal"), &data).unwrap();

        // --from the last checkpoint with no trailing records → empty
        let sliced = slice_records(read(dir.path()), None, Some("chk1"), None).unwrap();
        let changes = resolve(sliced).unwrap();
        assert!(changes.is_empty(), "no records after last chk: {changes:?}");
    }

    #[test]
    fn slice_not_found() {
        let dir = setup_test_dir();
        fs::write(dir.path().join("journal"), b"A\0\0a\0f\01\n").unwrap();
        assert!(slice_records(read(dir.path()), Some("nope"), None, None).is_err());
        assert!(slice_records(read(dir.path()), None, Some("nope"), None).is_err());
        assert!(slice_records(read(dir.path()), None, None, Some("nope")).is_err());
    }

    // -- Change method tests --

    #[test]
    fn change_ino() {
        assert_eq!(
            Change::Added {
                path: "/a".into(),
                ino: 42,
                dtype: DType::File
            }
            .ino(),
            Some(42)
        );
        assert_eq!(
            Change::Modified {
                path: "/a".into(),
                ino: 7,
                dtype: DType::File
            }
            .ino(),
            Some(7)
        );
        assert_eq!(Change::Deleted("/a".into()).ino(), None);
        assert_eq!(
            Change::Renamed {
                from: "/a".into(),
                to: "/b".into(),
                dtype: DType::File,
            }
            .ino(),
            None
        );
    }

    #[test]
    fn matches_path_added() {
        let c = Change::Added {
            path: "/src/main.rs".into(),
            ino: 1,
            dtype: DType::File,
        };
        assert!(c.matches_path("/src/main.rs"));
        assert!(!c.matches_path("/src/lib.rs"));
    }

    #[test]
    fn matches_path_modified() {
        let c = Change::Modified {
            path: "/etc/config".into(),
            ino: 5,
            dtype: DType::File,
        };
        assert!(c.matches_path("/etc/config"));
        assert!(!c.matches_path("/etc/other"));
    }

    #[test]
    fn matches_path_deleted() {
        let c = Change::Deleted("/old/file.txt".into());
        assert!(c.matches_path("/old/file.txt"));
        assert!(!c.matches_path("/old/other.txt"));
    }

    #[test]
    fn matches_path_renamed_from() {
        let c = Change::Renamed {
            from: "/a.txt".into(),
            to: "/b.txt".into(),
            dtype: DType::File,
        };
        assert!(c.matches_path("/a.txt"));
        assert!(c.matches_path("/b.txt"));
        assert!(!c.matches_path("/c.txt"));
    }

    // -- Redirect rename in segments --

    /// Base-file redirect rename across checkpoints shows correct incremental delta.
    #[test]
    fn segments_redirect_rename_across_checkpoints() {
        let dir = setup_test_dir();
        let mut data = Vec::new();
        // chk1: create file a
        data.extend_from_slice(b"A\0/nonexistent_test_12345\0a\0f\01\n");
        data.extend_from_slice(b"K\01\0chk1\n");
        // chk2: rename a->b via redirect
        data.extend_from_slice(b"D\0/nonexistent_test_12345\0a\n");
        data.extend_from_slice(b"R\0/nonexistent_test_12345\0b\0f\0/nonexistent_test_12345/a\n");
        data.extend_from_slice(b"K\02\0chk2\n");
        fs::write(dir.path().join("journal"), &data).unwrap();
        fs::write(dir.path().join("inodes/1"), "content").unwrap();

        let segments = resolve_segments(read(dir.path())).unwrap();
        assert_eq!(segments.len(), 1, "{segments:?}");

        // chk1→chk2: delta should show a Renamed(a -> b)
        let chk2 = &segments[0].changes;
        assert!(!chk2.is_empty(), "chk2 should have changes: {chk2:?}");
        let has_rename = chk2.iter().any(|c| {
            matches!(c, Change::Renamed { from, to, .. }
                if from == "/nonexistent_test_12345/a" && to == "/nonexistent_test_12345/b")
        });
        assert!(has_rename, "expected Renamed(a->b) in chk2, got: {chk2:?}");
    }

    /// DType must be preserved through redirect chains across segments.
    #[test]
    fn segments_preserve_dtype_through_redirect_chain() {
        let dir = setup_test_dir();
        let mut data = Vec::new();
        // chk1: create directory
        data.extend_from_slice(b"A\0/nonexistent_test_12345\0mydir\0d\01\n");
        data.extend_from_slice(b"K\01\0chk1\n");
        // chk2: rename mydir->dir2 via redirect (dtype=d)
        data.extend_from_slice(b"D\0/nonexistent_test_12345\0mydir\n");
        data.extend_from_slice(
            b"R\0/nonexistent_test_12345\0dir2\0d\0/nonexistent_test_12345/mydir\n",
        );
        data.extend_from_slice(b"K\02\0chk2\n");
        // chk3: rename dir2->dir3 via redirect (dtype=d, kernel follows chain)
        data.extend_from_slice(b"D\0/nonexistent_test_12345\0dir2\n");
        data.extend_from_slice(
            b"R\0/nonexistent_test_12345\0dir3\0d\0/nonexistent_test_12345/mydir\n",
        );
        data.extend_from_slice(b"K\03\0chk3\n");
        fs::write(dir.path().join("journal"), &data).unwrap();
        fs::create_dir_all(dir.path().join("inodes/1")).unwrap();

        let segments = resolve_segments(read(dir.path())).unwrap();
        assert_eq!(segments.len(), 2, "{segments:?}");

        // chk1→chk2: Renamed Dir (mydir -> dir2)
        let chk2_rename = segments[0].changes.iter().find(
            |c| matches!(c, Change::Renamed { to, .. } if to == "/nonexistent_test_12345/dir2"),
        );
        assert!(
            chk2_rename.is_some(),
            "chk2 should have rename to dir2: {:?}",
            segments[0].changes
        );
        assert!(
            matches!(
                chk2_rename.unwrap(),
                Change::Renamed {
                    dtype: DType::Dir,
                    ..
                }
            ),
            "chk2 rename should preserve DType::Dir, got: {:?}",
            chk2_rename.unwrap()
        );

        // chk2→chk3: Renamed Dir (dir2 -> dir3)
        let chk3_rename = segments[1].changes.iter().find(
            |c| matches!(c, Change::Renamed { to, .. } if to == "/nonexistent_test_12345/dir3"),
        );
        assert!(
            chk3_rename.is_some(),
            "chk3 should have rename to dir3: {:?}",
            segments[1].changes
        );
        assert!(
            matches!(
                chk3_rename.unwrap(),
                Change::Renamed {
                    dtype: DType::Dir,
                    ..
                }
            ),
            "chk3 rename should preserve DType::Dir, got: {:?}",
            chk3_rename.unwrap()
        );
    }

    // ── Rename edge cases ─────────────────────────────────────────────

    /// Rename + modify with different ino decomposes into Deleted + Modified.
    #[test]
    fn rename_modify_different_ino() {
        let dir = setup_test_dir();
        let mut data = Vec::new();
        // Rename a→b (redirect): delete a + redirect b→a
        data.extend_from_slice(b"D\0/nonexistent_test_12345\0a\n");
        data.extend_from_slice(b"R\0/nonexistent_test_12345\0b\0f\0/nonexistent_test_12345/a\n");
        // Modify b (COW): staged with new ino
        data.extend_from_slice(b"M\0/nonexistent_test_12345\0b\0f\02\n");
        fs::write(dir.path().join("journal"), &data).unwrap();
        fs::write(dir.path().join("inodes/2"), "modified").unwrap();

        let changes = resolve(read(dir.path())).unwrap();
        let has_deleted = changes.iter().any(|c| {
            matches!(c, Change::Deleted(p) if p == "/nonexistent_test_12345/a")
        });
        let has_modified = changes.iter().any(|c| {
            matches!(c, Change::Modified { path, ino: 2, .. }
                if path == "/nonexistent_test_12345/b")
        });
        assert!(has_deleted, "expected Deleted(a), got: {changes:?}");
        assert!(has_modified, "expected Modified(b, ino=2), got: {changes:?}");
    }

    /// Delete a rename destination: should undo the rename and delete the origin.
    #[test]
    fn delete_rename_destination_undoes_rename() {
        let dir = setup_test_dir();
        let mut data = Vec::new();
        // Rename /etc/hostname → /etc/hostname.bak
        data.extend_from_slice(b"D\0/etc\0hostname\n");
        data.extend_from_slice(b"R\0/etc\0hostname.bak\0f\0/etc/hostname\n");
        // Delete the destination /etc/hostname.bak
        data.extend_from_slice(b"D\0/etc\0hostname.bak\n");
        fs::write(dir.path().join("journal"), &data).unwrap();

        let changes = resolve(read(dir.path())).unwrap();
        // The rename was undone; the original file (/etc/hostname) should be deleted
        assert!(
            changes
                .iter()
                .any(|c| matches!(c, Change::Deleted(p) if p == "/etc/hostname")),
            "expected Deleted(/etc/hostname), got: {changes:?}"
        );
        assert!(
            !changes.iter().any(|c| matches!(c, Change::Renamed { .. })),
            "should have no renames, got: {changes:?}"
        );
    }

    /// Rename onto a path that was previously a rename destination:
    /// the overwritten rename's source should become a standalone delete.
    #[test]
    fn redirect_overwrites_previous_redirect() {
        let dir = setup_test_dir();
        let mut data = Vec::new();
        // Rename /etc/hostname → /etc/target
        data.extend_from_slice(b"D\0/etc\0hostname\n");
        data.extend_from_slice(b"R\0/etc\0target\0f\0/etc/hostname\n");
        // Rename /etc/hosts → /etc/target (overwrites the first rename's destination)
        data.extend_from_slice(b"D\0/etc\0hosts\n");
        data.extend_from_slice(b"R\0/etc\0target\0f\0/etc/hosts\n");
        fs::write(dir.path().join("journal"), &data).unwrap();

        let changes = resolve(read(dir.path())).unwrap();
        // Final: Renamed(hosts → target) + Deleted(hostname)
        let has_rename = changes.iter().any(|c| {
            matches!(c, Change::Renamed { from, to, .. }
            if from == "/etc/hosts" && to == "/etc/target")
        });
        let has_delete = changes
            .iter()
            .any(|c| matches!(c, Change::Deleted(p) if p == "/etc/hostname"));
        assert!(has_rename, "expected Renamed(hosts→target): {changes:?}");
        assert!(
            has_delete,
            "expected Deleted(hostname) from overwritten rename: {changes:?}"
        );
    }

    /// Create file, rename it, then create another file at the original name.
    /// Verifies staged-file rename (Delete + Staged with same ino) plus re-creation.
    #[test]
    fn staged_rename_then_recreate() {
        let dir = setup_test_dir();
        let mut data = Vec::new();
        // Create file at x (ino=1)
        data.extend_from_slice(b"A\0/nonexistent_test_12345\0x\0f\01\n");
        // Staged rename x→y: delete x + staged y with same ino
        data.extend_from_slice(b"D\0/nonexistent_test_12345\0x\n");
        data.extend_from_slice(b"A\0/nonexistent_test_12345\0y\0f\01\n");
        // Create new file at x (ino=2)
        data.extend_from_slice(b"A\0/nonexistent_test_12345\0x\0f\02\n");
        fs::write(dir.path().join("journal"), &data).unwrap();
        fs::write(dir.path().join("inodes/1"), "original").unwrap();
        fs::write(dir.path().join("inodes/2"), "replacement").unwrap();

        let changes = resolve(read(dir.path())).unwrap();
        assert_eq!(changes.len(), 2, "expected 2 changes, got: {changes:?}");
        let has_y = changes.iter().any(|c| {
            matches!(c, Change::Added { path, ino: 1, .. }
            if path == "/nonexistent_test_12345/y")
        });
        let has_x = changes.iter().any(|c| {
            matches!(c, Change::Added { path, ino: 2, .. }
            if path == "/nonexistent_test_12345/x")
        });
        assert!(has_y, "expected Added(y, ino=1): {changes:?}");
        assert!(has_x, "expected Added(x, ino=2): {changes:?}");
    }

    /// into_changes() must order: renames first, then adds/modifies, then deletes.
    /// commit.rs depends on this ordering so renames move base files before
    /// adds write to potentially overlapping paths.
    #[test]
    fn into_changes_ordering_renames_writes_deletes() {
        let dir = setup_test_dir();
        let mut data = Vec::new();
        // Add a new file (will become Added)
        data.extend_from_slice(b"A\0/nonexistent_test_12345\0new.txt\0f\01\n");
        // Delete an existing file (will become Deleted)
        data.extend_from_slice(b"D\0/etc\0hostname\n");
        // Rename a base file (will become Renamed)
        data.extend_from_slice(b"D\0/old\0path\n");
        data.extend_from_slice(b"R\0/new\0path\0f\0/old/path\n");
        fs::write(dir.path().join("journal"), &data).unwrap();
        fs::write(dir.path().join("inodes/1"), "content").unwrap();

        let changes = resolve(read(dir.path())).unwrap();
        assert_eq!(changes.len(), 3, "expected 3 changes, got: {changes:?}");

        assert!(
            matches!(&changes[0], Change::Renamed { .. }),
            "first change should be Renamed, got: {:?}",
            changes[0]
        );
        assert!(
            matches!(&changes[1], Change::Added { .. }),
            "second change should be Added, got: {:?}",
            changes[1]
        );
        assert!(
            matches!(&changes[2], Change::Deleted(_)),
            "third change should be Deleted, got: {:?}",
            changes[2]
        );
    }

    // ── reachable tests ────────────────────────────────────────────

    #[test]
    fn reachable_no_restores() {
        let records = vec![
            Record::Checkpoint(Checkpoint { gen_id: 1, name: "init".into() }),
            Record::Added { path: "/a".into(), dtype: Some(DType::File), ino: 1 },
            Record::Checkpoint(Checkpoint { gen_id: 2, name: "c2".into() }),
        ];
        let reachable = reachable(records.clone());
        assert_eq!(reachable.len(), 3);
    }

    #[test]
    fn reachable_single_restore() {
        // K1 [A] K2 [B] K3 S4(K2) [D] K5
        let records = vec![
            Record::Checkpoint(Checkpoint { gen_id: 1, name: "init".into() }),
            Record::Added { path: "/a".into(), dtype: Some(DType::File), ino: 1 },
            Record::Checkpoint(Checkpoint { gen_id: 2, name: "c2".into() }),
            Record::Added { path: "/b".into(), dtype: Some(DType::File), ino: 2 },
            Record::Checkpoint(Checkpoint { gen_id: 3, name: "c3".into() }),
            Record::Restore { gen_id: 4, target_gen: 2 },
            Record::Added { path: "/d".into(), dtype: Some(DType::File), ino: 3 },
            Record::Checkpoint(Checkpoint { gen_id: 5, name: "c5".into() }),
        ];
        let reachable = reachable(records);
        // Reachable: K1, A(/a), K2, A(/d), K5
        assert_eq!(reachable.len(), 5);
        assert!(matches!(&reachable[0], Record::Checkpoint(c) if c.gen_id == 1));
        assert!(matches!(&reachable[1], Record::Added { path, .. } if path == "/a"));
        assert!(matches!(&reachable[2], Record::Checkpoint(c) if c.gen_id == 2));
        assert!(matches!(&reachable[3], Record::Added { path, .. } if path == "/d"));
        assert!(matches!(&reachable[4], Record::Checkpoint(c) if c.gen_id == 5));
    }

    #[test]
    fn reachable_multiple_restores_last_wins() {
        // K1 [A] K2 [B] K3 S4(K2) [D] K5 S6(K1)
        let records = vec![
            Record::Checkpoint(Checkpoint { gen_id: 1, name: "init".into() }),
            Record::Added { path: "/a".into(), dtype: Some(DType::File), ino: 1 },
            Record::Checkpoint(Checkpoint { gen_id: 2, name: "c2".into() }),
            Record::Added { path: "/b".into(), dtype: Some(DType::File), ino: 2 },
            Record::Checkpoint(Checkpoint { gen_id: 3, name: "c3".into() }),
            Record::Restore { gen_id: 4, target_gen: 2 },
            Record::Added { path: "/d".into(), dtype: Some(DType::File), ino: 3 },
            Record::Checkpoint(Checkpoint { gen_id: 5, name: "c5".into() }),
            Record::Restore { gen_id: 6, target_gen: 1 },
        ];
        let reachable = reachable(records);
        // Reachable: K1 only
        assert_eq!(reachable.len(), 1);
        assert!(matches!(&reachable[0], Record::Checkpoint(c) if c.gen_id == 1));
    }

    #[test]
    fn reachable_nested_s_in_dead_zone() {
        // K1 [A] K2 [B] K3 S4(K1) [D] K5 [E] K6 S7(K5)
        let records = vec![
            Record::Checkpoint(Checkpoint { gen_id: 1, name: "init".into() }),
            Record::Added { path: "/a".into(), dtype: Some(DType::File), ino: 1 },
            Record::Checkpoint(Checkpoint { gen_id: 2, name: "c2".into() }),
            Record::Added { path: "/b".into(), dtype: Some(DType::File), ino: 2 },
            Record::Checkpoint(Checkpoint { gen_id: 3, name: "c3".into() }),
            Record::Restore { gen_id: 4, target_gen: 1 },
            Record::Added { path: "/d".into(), dtype: Some(DType::File), ino: 3 },
            Record::Checkpoint(Checkpoint { gen_id: 5, name: "c5".into() }),
            Record::Added { path: "/e".into(), dtype: Some(DType::File), ino: 4 },
            Record::Checkpoint(Checkpoint { gen_id: 6, name: "c6".into() }),
            Record::Restore { gen_id: 7, target_gen: 5 },
        ];
        let reachable = reachable(records);
        // S7(K5): reachable prefix up to K5, nothing after S7
        // But prefix K1..K5 contains S4(K1), so recurse:
        //   S4(K1): reachable prefix up to K1, then K5 suffix = [D] K5
        // Final: K1, [D], K5
        assert_eq!(reachable.len(), 3);
        assert!(matches!(&reachable[0], Record::Checkpoint(c) if c.gen_id == 1));
        assert!(matches!(&reachable[1], Record::Added { path, .. } if path == "/d"));
        assert!(matches!(&reachable[2], Record::Checkpoint(c) if c.gen_id == 5));
    }

    #[test]
    fn reachable_undo_restore() {
        // K1 [A] K2 [B] K3 S4(K1) [D] K5 S6(K3)
        let records = vec![
            Record::Checkpoint(Checkpoint { gen_id: 1, name: "init".into() }),
            Record::Added { path: "/a".into(), dtype: Some(DType::File), ino: 1 },
            Record::Checkpoint(Checkpoint { gen_id: 2, name: "c2".into() }),
            Record::Added { path: "/b".into(), dtype: Some(DType::File), ino: 2 },
            Record::Checkpoint(Checkpoint { gen_id: 3, name: "c3".into() }),
            Record::Restore { gen_id: 4, target_gen: 1 },
            Record::Added { path: "/d".into(), dtype: Some(DType::File), ino: 3 },
            Record::Checkpoint(Checkpoint { gen_id: 5, name: "c5".into() }),
            Record::Restore { gen_id: 6, target_gen: 3 },
        ];
        let reachable = reachable(records);
        // S6(K3) is last S. Reachable = records[0..=K3] (K3 is at idx 4)
        // No S in that prefix. Reachable: K1 [A] K2 [B] K3
        assert_eq!(reachable.len(), 5);
        assert!(matches!(&reachable[0], Record::Checkpoint(c) if c.gen_id == 1));
        assert!(matches!(&reachable[1], Record::Added { path, .. } if path == "/a"));
        assert!(matches!(&reachable[2], Record::Checkpoint(c) if c.gen_id == 2));
        assert!(matches!(&reachable[3], Record::Added { path, .. } if path == "/b"));
        assert!(matches!(&reachable[4], Record::Checkpoint(c) if c.gen_id == 3));
    }

    #[test]
    fn reachable_restore_to_initial() {
        // K1 [A] K2 S3(K1)
        let records = vec![
            Record::Checkpoint(Checkpoint { gen_id: 1, name: "init".into() }),
            Record::Added { path: "/a".into(), dtype: Some(DType::File), ino: 1 },
            Record::Checkpoint(Checkpoint { gen_id: 2, name: "c2".into() }),
            Record::Restore { gen_id: 3, target_gen: 1 },
        ];
        let reachable = reachable(records);
        // Reachable: K1 only
        assert_eq!(reachable.len(), 1);
        assert!(matches!(&reachable[0], Record::Checkpoint(c) if c.gen_id == 1));
    }

    #[test]
    fn reachable_corrupt_s_record_skipped() {
        // S record references non-existent checkpoint gen 99 — should be skipped.
        let records = vec![
            Record::Checkpoint(Checkpoint { gen_id: 1, name: "init".into() }),
            Record::Added { path: "/a".into(), dtype: Some(DType::File), ino: 1 },
            Record::Checkpoint(Checkpoint { gen_id: 2, name: "c2".into() }),
            Record::Restore { gen_id: 3, target_gen: 99 },
            Record::Added { path: "/b".into(), dtype: Some(DType::File), ino: 2 },
        ];
        let reachable = reachable(records);
        // Corrupt S is skipped, all records pass through.
        assert_eq!(reachable.len(), 5);
    }

    #[test]
    fn reachable_restore_then_work_then_commit_resolves() {
        // K1 A(/x,1) K2 M(/x,2) K3 S4(K2) A(/y,3)
        let records = vec![
            Record::Checkpoint(Checkpoint { gen_id: 1, name: "init".into() }),
            Record::Added { path: "/x".into(), dtype: Some(DType::File), ino: 1 },
            Record::Checkpoint(Checkpoint { gen_id: 2, name: "c2".into() }),
            Record::Modified { path: "/x".into(), dtype: Some(DType::File), ino: 2 },
            Record::Checkpoint(Checkpoint { gen_id: 3, name: "c3".into() }),
            Record::Restore { gen_id: 4, target_gen: 2 },
            Record::Added { path: "/y".into(), dtype: Some(DType::File), ino: 3 },
        ];
        let reachable = reachable(records);
        let changes = resolve(reachable).unwrap();
        // After restore to K2: /x is ino 1 (from K1→K2 segment).
        // Then /y is added. So: Added(/x, 1) + Added(/y, 3)
        assert_eq!(changes.len(), 2);
        assert!(changes
            .iter()
            .any(|c| matches!(c, Change::Added { path, ino: 1, .. } if path == "/x")));
        assert!(changes
            .iter()
            .any(|c| matches!(c, Change::Added { path, ino: 3, .. } if path == "/y")));
    }

    #[test]
    fn resolve_segments_after_reachable() {
        // K1 A(/x,1) K2 A(/y,2) K3 S4(K2) A(/z,3) K5
        let records = vec![
            Record::Checkpoint(Checkpoint { gen_id: 1, name: "init".into() }),
            Record::Added { path: "/x".into(), dtype: Some(DType::File), ino: 1 },
            Record::Checkpoint(Checkpoint { gen_id: 2, name: "c2".into() }),
            Record::Added { path: "/y".into(), dtype: Some(DType::File), ino: 2 },
            Record::Checkpoint(Checkpoint { gen_id: 3, name: "c3".into() }),
            Record::Restore { gen_id: 4, target_gen: 2 },
            Record::Added { path: "/z".into(), dtype: Some(DType::File), ino: 3 },
            Record::Checkpoint(Checkpoint { gen_id: 5, name: "c5".into() }),
        ];
        let reachable = reachable(records);
        let segments = resolve_segments(reachable).unwrap();
        // Reachable: K1, A(/x), K2, A(/z), K5
        // Segments: K1→K2 (A /x), K2→K5 (A /z)
        assert_eq!(segments.len(), 2);
        assert_eq!(segments[0].from.gen_id, 1);
        assert_eq!(segments[0].to.as_ref().unwrap().gen_id, 2);
        assert_eq!(segments[0].changes.len(), 1);
        assert_eq!(segments[1].from.gen_id, 2);
        assert_eq!(segments[1].to.as_ref().unwrap().gen_id, 5);
        assert_eq!(segments[1].changes.len(), 1);
    }

    #[test]
    fn reachable_empty_journal() {
        let reachable = reachable(vec![]);
        assert!(reachable.is_empty());
    }

    #[test]
    fn reachable_consecutive_s_records() {
        // K1 [A] K2 [B] K3 S4(K2) S5(K1)
        // Two consecutive restores: second one "wins" and goes further back.
        let records = vec![
            Record::Checkpoint(Checkpoint { gen_id: 1, name: "init".into() }),
            Record::Added { path: "/a".into(), dtype: Some(DType::File), ino: 1 },
            Record::Checkpoint(Checkpoint { gen_id: 2, name: "c2".into() }),
            Record::Added { path: "/b".into(), dtype: Some(DType::File), ino: 2 },
            Record::Checkpoint(Checkpoint { gen_id: 3, name: "c3".into() }),
            Record::Restore { gen_id: 4, target_gen: 2 },
            Record::Restore { gen_id: 5, target_gen: 1 },
        ];
        let reachable = reachable(records);
        // S5(K1) kills everything after K1. S4(K2) is in that dead zone.
        assert_eq!(reachable.len(), 1);
        assert!(matches!(&reachable[0], Record::Checkpoint(c) if c.gen_id == 1));
    }

    #[test]
    fn resolve_segments_trailing_s_only() {
        // K1 [A] K2 S3(K1) — trailing segment after reachable has no
        // mutations (the S record is filtered out). Should produce one segment
        // (K1→K2 with A) and no trailing segment.
        let records = vec![
            Record::Checkpoint(Checkpoint { gen_id: 1, name: "init".into() }),
            Record::Added { path: "/a".into(), dtype: Some(DType::File), ino: 1 },
            Record::Checkpoint(Checkpoint { gen_id: 2, name: "c2".into() }),
            Record::Restore { gen_id: 3, target_gen: 1 },
        ];
        let reachable = reachable(records);
        // Reachable: K1 only (S3 kills everything after K1)
        let segments = resolve_segments(reachable).unwrap();
        // Only the initial checkpoint — no mutations, no segments.
        assert!(segments.is_empty());
    }
}

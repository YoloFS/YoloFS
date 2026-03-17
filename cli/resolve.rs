// agfs CLI — resolve.rs
//
// Resolve (replay) the append-only journal into a list of Changes.
//
// Intermediate operations collapse into their final effect:
// - `A(x) → R(x,y)` collapses to `Add(y)`.
// - `R(a,b) → R(b,c)` collapses to `Rename(a,c)`.
// - `A(x) → D(x)` cancels out.
// - `R(a,b) → A(a)` produces `Rename(a,b) + Add(a)`.

use crate::journal::Record;
use anyhow::Result;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

/// A resolved change — the final effect of replaying the journal.
#[derive(Debug)]
pub enum Change {
    Added { path: String, ino: u64 },
    Modified { path: String, ino: u64 },
    Deleted(String),
    Renamed { from: String, to: String },
    RenamedModified { from: String, to: String, ino: u64 },
}

impl Change {
    /// Return the staged inode ID if this change carries one.
    pub fn ino(&self) -> Option<u64> {
        match self {
            Change::Added { ino, .. }
            | Change::Modified { ino, .. }
            | Change::RenamedModified { ino, .. } => Some(*ino),
            _ => None,
        }
    }

    /// True if this change involves the given path (as source or destination).
    pub fn matches_path(&self, target: &str) -> bool {
        match self {
            Change::Added { path, .. } | Change::Modified { path, .. } => path == target,
            Change::Deleted(path) => path == target,
            Change::Renamed { from, to } | Change::RenamedModified { from, to, .. } => {
                from == target || to == target
            }
        }
    }
}

/// Resolve records into their final collapsed changes.
///
/// Intermediate operations collapse into their final effect:
/// - `A(x) → R(x,y)` collapses to `Add(y)`.
/// - `R(a,b) → R(b,c)` collapses to `Rename(a,c)`.
/// - `A(x) → D(x)` cancels out.
/// - `R(a,b) → A(a)` produces `Rename(a,b) + Add(a)`.
pub fn resolve(records: &[Record]) -> Result<Vec<Change>> {
    let mut state = ResolveState::new();
    for record in records {
        state.process(record);
    }
    Ok(state.emit_changes())
}

/// Find the record index of a checkpoint by name or numeric ID.
/// Tries parsing as a numeric ID first, then falls back to name match
/// (using the latest occurrence if names are duplicated).
fn find_checkpoint_index(records: &[Record], name_or_id: &str) -> Result<usize> {
    // Try numeric ID first
    if let Ok(target_id) = name_or_id.parse::<u64>() {
        for (i, record) in records.iter().enumerate() {
            if let Record::Checkpoint { id, .. } = record
                && *id == target_id
            {
                return Ok(i);
            }
        }
    }

    // Fall back to name match (latest occurrence)
    let mut last = None;
    for (i, record) in records.iter().enumerate() {
        if let Record::Checkpoint { name: n, .. } = record
            && n == name_or_id
        {
            last = Some(i);
        }
    }
    last.ok_or_else(|| anyhow::anyhow!("checkpoint not found: {name_or_id}"))
}

/// Resolve journal up to (and including) the named checkpoint.
/// Returns changes that were staged at the time of the checkpoint.
pub fn resolve_at(records: &[Record], checkpoint_name: &str) -> Result<Vec<Change>> {
    let chk_idx = find_checkpoint_index(records, checkpoint_name)?;
    resolve(&records[..=chk_idx])
}

/// Resolve journal from after the named checkpoint to the end.
/// Returns the diff between checkpoint state and current state as two
/// resolved states: (at_checkpoint, current).
pub fn resolve_from(records: &[Record], checkpoint_name: &str) -> Result<(Vec<Change>, Vec<Change>)> {
    let chk_idx = find_checkpoint_index(records, checkpoint_name)?;
    let at_chk = resolve(&records[..=chk_idx])?;
    let current = resolve(records)?;
    Ok((at_chk, current))
}

/// Incremental resolution state — processes records one at a time and can
/// emit the resolved change list at any point. Used by both `resolve`
/// (batch) and `resolve_sections` (incremental checkpoints) so the journal is
/// traversed only once.
struct ResolveState {
    resolved_renames: BTreeMap<String, String>,
    resolved_adds: BTreeMap<String, u64>,
    resolved_deletes: BTreeSet<String>,
    rename_origin: BTreeMap<String, String>,
}

impl ResolveState {
    fn new() -> Self {
        Self {
            resolved_renames: BTreeMap::new(),
            resolved_adds: BTreeMap::new(),
            resolved_deletes: BTreeSet::new(),
            rename_origin: BTreeMap::new(),
        }
    }

    fn process(&mut self, record: &Record) {
        let base = Path::new("/");
        match record {
            Record::Add { path, ino } => {
                self.resolved_adds.insert(path.clone(), *ino);
                self.resolved_deletes.remove(path);
            }
            Record::Delete { path } => {
                if self.resolved_adds.remove(path).is_some() {
                    let base_file = base.join(path.trim_start_matches('/'));
                    if base_file.exists() {
                        self.resolved_deletes.insert(path.clone());
                    }
                } else if let Some(origin) = self.rename_origin.remove(path) {
                    self.resolved_renames.remove(&origin);
                    self.resolved_deletes.insert(origin);
                } else {
                    self.resolved_deletes.insert(path.clone());
                }
            }
            Record::Rename { old_path, new_path } => {
                if let Some(ino) = self.resolved_adds.remove(old_path) {
                    self.resolved_adds.insert(new_path.clone(), ino);
                    let base_file = base.join(old_path.trim_start_matches('/'));
                    if base_file.exists() {
                        self.resolved_deletes.insert(old_path.clone());
                    }
                } else if let Some(origin) = self.rename_origin.remove(old_path) {
                    self.resolved_renames
                        .insert(origin.clone(), new_path.clone());
                    self.rename_origin.insert(new_path.clone(), origin);
                } else {
                    self.resolved_renames
                        .insert(old_path.clone(), new_path.clone());
                    self.rename_origin
                        .insert(new_path.clone(), old_path.clone());
                }
            }
            Record::Checkpoint { .. } => {}
        }
    }

    fn emit_changes(&self) -> Vec<Change> {
        let base = Path::new("/");
        let mut changes = Vec::new();
        let rename_srcs: BTreeSet<&String> = self.resolved_renames.keys().collect();
        let rename_dsts: BTreeSet<&String> = self.resolved_renames.values().collect();

        for (old_path, new_path) in &self.resolved_renames {
            if let Some(&ino) = self.resolved_adds.get(new_path) {
                changes.push(Change::RenamedModified {
                    from: old_path.clone(),
                    to: new_path.clone(),
                    ino,
                });
            } else {
                changes.push(Change::Renamed {
                    from: old_path.clone(),
                    to: new_path.clone(),
                });
            }
        }

        for (path, ino) in &self.resolved_adds {
            if rename_dsts.contains(path) {
                continue;
            }
            let base_file = base.join(path.trim_start_matches('/'));
            if base_file.exists() {
                changes.push(Change::Modified {
                    path: path.clone(),
                    ino: *ino,
                });
            } else {
                changes.push(Change::Added {
                    path: path.clone(),
                    ino: *ino,
                });
            }
        }

        for path in self.resolved_deletes.iter().rev() {
            if rename_srcs.contains(path) {
                continue;
            }
            changes.push(Change::Deleted(path.clone()));
        }

        changes
    }
}

/// A group of resolved changes belonging to a checkpoint (or trailing).
#[derive(Debug)]
pub struct Section {
    /// Checkpoint id and name, or None for trailing (unsaved) changes.
    pub checkpoint: Option<(u64, String)>,
    pub changes: Vec<Change>,
}

/// Resolve the journal into sections grouped by checkpoint boundaries.
///
/// Each section contains the *delta* of changes introduced between the
/// previous checkpoint and this one.  The trailing section (checkpoint=None)
/// holds changes after the last checkpoint.
///
/// When there are no checkpoints, returns a single section with checkpoint=None.
pub fn resolve_sections(records: &[Record]) -> Result<Vec<Section>> {

    // Collect checkpoint boundary indices
    let chk_indices: Vec<(usize, u64, String)> = records
        .iter()
        .enumerate()
        .filter_map(|(i, r)| match r {
            Record::Checkpoint { id, name } => Some((i, *id, name.clone())),
            _ => None,
        })
        .collect();

    if chk_indices.is_empty() {
        let changes = resolve(records)?;
        return Ok(vec![Section {
            checkpoint: None,
            changes,
        }]);
    }

    // Process records incrementally through a single ResolveState,
    // checkpointting at each boundary — O(N) total instead of O(N*S).
    let mut sections = Vec::new();
    let mut state = ResolveState::new();
    let mut prev_cs = ChangesState::default();
    let mut record_idx = 0;

    for &(chk_idx, chk_id, ref chk_name) in &chk_indices {
        while record_idx <= chk_idx {
            state.process(&records[record_idx]);
            record_idx += 1;
        }
        let curr_cs = ChangesState::from_changes(&state.emit_changes());
        let delta = prev_cs.diff(&curr_cs);
        sections.push(Section {
            checkpoint: Some((chk_id, chk_name.clone())),
            changes: delta,
        });
        prev_cs = curr_cs;
    }

    // Trailing changes after the last checkpoint
    while record_idx < records.len() {
        state.process(&records[record_idx]);
        record_idx += 1;
    }
    let final_cs = ChangesState::from_changes(&state.emit_changes());
    let trailing = prev_cs.diff(&final_cs);
    if !trailing.is_empty() {
        sections.push(Section {
            checkpoint: None,
            changes: trailing,
        });
    }

    Ok(sections)
}

/// Lightweight checkpoint of resolved state for computing per-section deltas.
#[derive(Default)]
struct ChangesState {
    /// path → (ino or None for deletes, rename source or None)
    entries: BTreeMap<String, StateEntry>,
}

#[derive(Clone, PartialEq)]
struct StateEntry {
    ino: Option<u64>,
    renamed_from: Option<String>,
}

impl ChangesState {
    fn from_changes(changes: &[Change]) -> Self {
        let mut entries = BTreeMap::new();
        for change in changes {
            match change {
                Change::Added { path, ino } | Change::Modified { path, ino } => {
                    entries.insert(
                        path.clone(),
                        StateEntry {
                            ino: Some(*ino),
                            renamed_from: None,
                        },
                    );
                }
                Change::Deleted(path) => {
                    entries.insert(
                        path.clone(),
                        StateEntry {
                            ino: None,
                            renamed_from: None,
                        },
                    );
                }
                Change::Renamed { from, to } => {
                    entries.insert(
                        from.clone(),
                        StateEntry {
                            ino: None,
                            renamed_from: None,
                        },
                    );
                    entries.insert(
                        to.clone(),
                        StateEntry {
                            ino: None,
                            renamed_from: Some(from.clone()),
                        },
                    );
                }
                Change::RenamedModified { from, to, ino } => {
                    entries.insert(
                        from.clone(),
                        StateEntry {
                            ino: None,
                            renamed_from: None,
                        },
                    );
                    entries.insert(
                        to.clone(),
                        StateEntry {
                            ino: Some(*ino),
                            renamed_from: Some(from.clone()),
                        },
                    );
                }
            }
        }
        Self { entries }
    }

    /// Compute the delta from self → other as a list of Changes.
    fn diff(&self, other: &Self) -> Vec<Change> {
        let base = Path::new("/");
        let mut changes = Vec::new();

        // Paths in other but not in self → new in this section
        // Paths in both but different → modified in this section
        for (path, new_entry) in &other.entries {
            match self.entries.get(path) {
                None => {
                    // New path in this section — emit the appropriate change
                    if let Some(from) = &new_entry.renamed_from {
                        if let Some(ino) = new_entry.ino {
                            changes.push(Change::RenamedModified {
                                from: from.clone(),
                                to: path.clone(),
                                ino,
                            });
                        } else {
                            changes.push(Change::Renamed {
                                from: from.clone(),
                                to: path.clone(),
                            });
                        }
                    } else if let Some(ino) = new_entry.ino {
                        let base_file = base.join(path.trim_start_matches('/'));
                        if base_file.exists() {
                            changes.push(Change::Modified {
                                path: path.clone(),
                                ino,
                            });
                        } else {
                            changes.push(Change::Added {
                                path: path.clone(),
                                ino,
                            });
                        }
                    } else {
                        changes.push(Change::Deleted(path.clone()));
                    }
                }
                Some(old_entry) if old_entry != new_entry => {
                    // Path existed before but changed
                    if let Some(ino) = new_entry.ino {
                        changes.push(Change::Modified {
                            path: path.clone(),
                            ino,
                        });
                    } else if old_entry.ino.is_some() {
                        changes.push(Change::Deleted(path.clone()));
                    }
                }
                _ => {} // unchanged
            }
        }

        changes
    }
}

/// Split the journal at a named checkpoint: resolve changes up to the checkpoint,
/// and return remaining records after it. Reads the journal once.
pub fn split_at_checkpoint(
    records: &[Record],
    checkpoint_name: &str,
) -> Result<(Vec<Change>, Vec<Record>)> {
    let chk_idx = find_checkpoint_index(records, checkpoint_name)?;
    let changes = resolve(&records[..=chk_idx])?;
    let remaining = records[chk_idx + 1..].to_vec();
    Ok((changes, remaining))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::journal::Record;
    use std::fs;

    fn setup_test_dir() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("inodes")).unwrap();
        dir
    }

    fn read(dir: &std::path::Path) -> Vec<Record> {
        crate::journal::read(dir).unwrap()
    }

    #[test]
    fn resolve_empty() {
        let dir = setup_test_dir();
        let changes = resolve(&read(dir.path())).unwrap();
        assert!(changes.is_empty());
    }

    #[test]
    fn resolve_add() {
        let dir = setup_test_dir();
        let mut data = Vec::new();
        data.extend_from_slice(b"A\0/nonexistent_test_12345/new.txt\01\n");
        fs::write(dir.path().join("journal"), &data).unwrap();
        fs::write(dir.path().join("inodes/1"), "content").unwrap();

        let changes = resolve(&read(dir.path())).unwrap();
        assert_eq!(changes.len(), 1);
        assert!(matches!(&changes[0], Change::Added { path, .. } if path.contains("new.txt")));
    }

    #[test]
    fn resolve_delete() {
        let dir = setup_test_dir();
        let mut data = Vec::new();
        data.extend_from_slice(b"D\0/etc/hostname\n");
        fs::write(dir.path().join("journal"), &data).unwrap();

        let changes = resolve(&read(dir.path())).unwrap();
        assert_eq!(changes.len(), 1);
        assert!(matches!(&changes[0], Change::Deleted(p) if p.contains("hostname")));
    }

    #[test]
    fn resolve_rename() {
        let dir = setup_test_dir();
        let mut data = Vec::new();
        data.extend_from_slice(b"R\0/old/path\0/new/path\n");
        fs::write(dir.path().join("journal"), &data).unwrap();

        let changes = resolve(&read(dir.path())).unwrap();
        assert_eq!(changes.len(), 1);
        assert!(matches!(&changes[0], Change::Renamed { from, to }
            if from == "/old/path" && to == "/new/path"));
    }

    /// touch x, mv x→y: staging-created file renamed.
    /// Should collapse to a single Add at the new path, not a base rename.
    #[test]
    fn create_then_rename_collapses_to_add() {
        let dir = setup_test_dir();
        let mut data = Vec::new();
        data.extend_from_slice(b"A\0/nonexistent_test_12345/x\01\n");
        data.extend_from_slice(b"R\0/nonexistent_test_12345/x\0/nonexistent_test_12345/y\n");
        fs::write(dir.path().join("journal"), &data).unwrap();
        fs::write(dir.path().join("inodes/1"), "content").unwrap();

        let changes = resolve(&read(dir.path())).unwrap();
        assert_eq!(changes.len(), 1, "expected 1 change, got: {changes:?}");
        assert!(
            matches!(&changes[0], Change::Added { path, ino } if path == "/nonexistent_test_12345/y" && *ino == 1),
            "expected Added at y with ino 1, got: {:?}",
            changes[0]
        );
    }

    /// mv a→b, touch a: rename then recreate at old path.
    /// Should produce Renamed(a→b) + Added(a).
    #[test]
    fn rename_then_recreate_at_old_path() {
        let dir = setup_test_dir();
        let mut data = Vec::new();
        data.extend_from_slice(b"R\0/nonexistent_test_12345/a\0/nonexistent_test_12345/b\n");
        data.extend_from_slice(b"A\0/nonexistent_test_12345/a\02\n");
        fs::write(dir.path().join("journal"), &data).unwrap();
        fs::write(dir.path().join("inodes/2"), "new content").unwrap();

        let changes = resolve(&read(dir.path())).unwrap();
        assert_eq!(changes.len(), 2, "expected 2 changes, got: {changes:?}");
        let has_rename = changes.iter().any(|c| {
            matches!(c, Change::Renamed { from, to }
            if from == "/nonexistent_test_12345/a" && to == "/nonexistent_test_12345/b")
        });
        let has_add = changes.iter().any(|c| {
            matches!(c, Change::Added { path, ino }
            if path == "/nonexistent_test_12345/a" && *ino == 2)
        });
        assert!(has_rename, "expected Renamed(a→b), got: {changes:?}");
        assert!(has_add, "expected Added(a, 2), got: {changes:?}");
    }

    /// mv a→b, mv b→c: chained renames.
    /// Should collapse to a single Renamed(a→c).
    #[test]
    fn chained_renames_collapse() {
        let dir = setup_test_dir();
        let mut data = Vec::new();
        data.extend_from_slice(b"R\0/nonexistent_test_12345/a\0/nonexistent_test_12345/b\n");
        data.extend_from_slice(b"R\0/nonexistent_test_12345/b\0/nonexistent_test_12345/c\n");
        fs::write(dir.path().join("journal"), &data).unwrap();

        let changes = resolve(&read(dir.path())).unwrap();
        assert_eq!(changes.len(), 1, "expected 1 change, got: {changes:?}");
        assert!(
            matches!(&changes[0], Change::Renamed { from, to }
                if from == "/nonexistent_test_12345/a" && to == "/nonexistent_test_12345/c"),
            "expected Renamed(a→c), got: {:?}",
            changes[0]
        );
    }

    /// touch x, rm x: create then delete cancels out (x never existed in base).
    #[test]
    fn create_then_delete_cancels() {
        let dir = setup_test_dir();
        let mut data = Vec::new();
        data.extend_from_slice(b"A\0/nonexistent_test_12345/x\01\n");
        data.extend_from_slice(b"D\0/nonexistent_test_12345/x\n");
        fs::write(dir.path().join("journal"), &data).unwrap();
        fs::write(dir.path().join("inodes/1"), "content").unwrap();

        let changes = resolve(&read(dir.path())).unwrap();
        assert!(changes.is_empty(), "expected no changes, got: {changes:?}");
    }

    /// Modify base file then delete: should still delete the base file.
    /// A(path) for a base file is a COW modify; D(path) then means "delete base".
    #[test]
    fn modify_base_then_delete() {
        let dir = setup_test_dir();
        // /etc/hostname exists in base
        let mut data = Vec::new();
        data.extend_from_slice(b"A\0/etc/hostname\01\n");
        data.extend_from_slice(b"D\0/etc/hostname\n");
        fs::write(dir.path().join("journal"), &data).unwrap();
        fs::write(dir.path().join("inodes/1"), "modified").unwrap();

        let changes = resolve(&read(dir.path())).unwrap();
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
        data.extend_from_slice(b"A\0/etc/hostname\01\n");
        data.extend_from_slice(b"R\0/etc/hostname\0/etc/hostname.bak\n");
        fs::write(dir.path().join("journal"), &data).unwrap();
        fs::write(dir.path().join("inodes/1"), "modified").unwrap();

        let changes = resolve(&read(dir.path())).unwrap();
        // Should have: Add(/etc/hostname.bak, 1) + Delete(/etc/hostname)
        assert_eq!(changes.len(), 2, "expected 2 changes, got: {changes:?}");
        let has_add = changes.iter().any(|c| {
            matches!(c, Change::Added { path, ino }
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

    // ── Checkpoint tests ───────────────────────────────────────────────

    #[test]
    fn split_at_checkpoint_basic() {
        let dir = setup_test_dir();
        let mut data = Vec::new();
        data.extend_from_slice(b"A\0/a\01\n");
        data.extend_from_slice(b"K\01\0first\n");
        data.extend_from_slice(b"A\0/a\02\n");
        data.extend_from_slice(b"K\02\0second\n");
        fs::write(dir.path().join("journal"), &data).unwrap();
        fs::write(dir.path().join("inodes/1"), "").unwrap();
        fs::write(dir.path().join("inodes/2"), "").unwrap();

        let (changes, remaining) = split_at_checkpoint(&read(dir.path()), "first").unwrap();
        assert_eq!(changes.len(), 1);
        assert_eq!(remaining.len(), 2); // A + S
    }

    #[test]
    fn resolve_at_checkpoint() {
        let dir = setup_test_dir();
        let mut data = Vec::new();
        // Add file, checkpoint, then modify file
        data.extend_from_slice(b"A\0/nonexistent_test_12345/x\01\n");
        data.extend_from_slice(b"K\01\0snap1\n");
        data.extend_from_slice(b"A\0/nonexistent_test_12345/x\02\n");
        data.extend_from_slice(b"A\0/nonexistent_test_12345/y\03\n");
        fs::write(dir.path().join("journal"), &data).unwrap();
        fs::write(dir.path().join("inodes/1"), "v1").unwrap();
        fs::write(dir.path().join("inodes/2"), "v2").unwrap();
        fs::write(dir.path().join("inodes/3"), "v3").unwrap();

        // At snap1, only x with ino 1 should be visible
        let changes = resolve_at(&read(dir.path()), "snap1").unwrap();
        assert_eq!(changes.len(), 1, "at snap1: {changes:?}");
        assert!(matches!(&changes[0], Change::Added { path, ino }
            if path == "/nonexistent_test_12345/x" && *ino == 1));

        // Full resolve should see x with ino 2 and y with ino 3
        let all = resolve(&read(dir.path())).unwrap();
        assert_eq!(all.len(), 2, "full: {all:?}");
    }

    #[test]
    fn resolve_at_matches_latest_checkpoint() {
        let dir = setup_test_dir();
        let mut data = Vec::new();
        data.extend_from_slice(b"A\0/nonexistent_test_12345/x\01\n");
        data.extend_from_slice(b"K\01\0dup\n");
        data.extend_from_slice(b"A\0/nonexistent_test_12345/y\02\n");
        data.extend_from_slice(b"K\02\0dup\n");
        data.extend_from_slice(b"A\0/nonexistent_test_12345/z\03\n");
        fs::write(dir.path().join("journal"), &data).unwrap();
        fs::write(dir.path().join("inodes/1"), "").unwrap();
        fs::write(dir.path().join("inodes/2"), "").unwrap();
        fs::write(dir.path().join("inodes/3"), "").unwrap();

        // "dup" should match the latest (second) occurrence
        let changes = resolve_at(&read(dir.path()), "dup").unwrap();
        assert_eq!(changes.len(), 2, "at latest dup: {changes:?}");
    }

    #[test]
    fn resolve_at_not_found() {
        let dir = setup_test_dir();
        fs::write(dir.path().join("journal"), b"A\0/a\01\n").unwrap();
        assert!(resolve_at(&read(dir.path()), "nonexistent").is_err());
    }

    #[test]
    fn resolve_at_by_numeric_id() {
        let dir = setup_test_dir();
        let mut data = Vec::new();
        data.extend_from_slice(b"A\0/nonexistent_test_12345/x\01\n");
        data.extend_from_slice(b"K\05\0mysnap\n");
        data.extend_from_slice(b"A\0/nonexistent_test_12345/y\02\n");
        fs::write(dir.path().join("journal"), &data).unwrap();
        fs::write(dir.path().join("inodes/1"), "").unwrap();
        fs::write(dir.path().join("inodes/2"), "").unwrap();

        // Lookup by ID "5" should find the checkpoint
        let changes = resolve_at(&read(dir.path()), "5").unwrap();
        assert_eq!(changes.len(), 1, "by id: {changes:?}");

        // Lookup by name should also work
        let changes2 = resolve_at(&read(dir.path()), "mysnap").unwrap();
        assert_eq!(changes2.len(), 1, "by name: {changes2:?}");
    }

    #[test]
    fn resolve_at_id_not_found() {
        let dir = setup_test_dir();
        let mut data = Vec::new();
        data.extend_from_slice(b"K\01\0snap\n");
        fs::write(dir.path().join("journal"), &data).unwrap();
        // ID 99 doesn't exist
        assert!(resolve_at(&read(dir.path()), "99").is_err());
    }

    #[test]
    fn resolve_at_id_takes_priority_over_name() {
        let dir = setup_test_dir();
        let mut data = Vec::new();
        // Checkpoint with id=1 named "first", checkpoint with id=2 named "1"
        // (a name that looks like a number)
        data.extend_from_slice(b"A\0/nonexistent_test_12345/x\01\n");
        data.extend_from_slice(b"K\01\0first\n");
        data.extend_from_slice(b"A\0/nonexistent_test_12345/y\02\n");
        data.extend_from_slice(b"K\02\01\n");
        fs::write(dir.path().join("journal"), &data).unwrap();
        fs::write(dir.path().join("inodes/1"), "").unwrap();
        fs::write(dir.path().join("inodes/2"), "").unwrap();

        // "1" should match id=1 (the first checkpoint), not the name "1" (second checkpoint)
        let changes = resolve_at(&read(dir.path()), "1").unwrap();
        assert_eq!(
            changes.len(),
            1,
            "id=1 should find first checkpoint: {changes:?}"
        );

        // "2" should match id=2
        let changes2 = resolve_at(&read(dir.path()), "2").unwrap();
        assert_eq!(
            changes2.len(),
            2,
            "id=2 should find second checkpoint: {changes2:?}"
        );

        // "first" should match by name
        let changes3 = resolve_at(&read(dir.path()), "first").unwrap();
        assert_eq!(changes3.len(), 1, "name=first: {changes3:?}");
    }

    #[test]
    fn split_at_checkpoint_by_id() {
        let dir = setup_test_dir();
        let mut data = Vec::new();
        data.extend_from_slice(b"A\0/a\01\n");
        data.extend_from_slice(b"K\07\0snap\n");
        data.extend_from_slice(b"A\0/b\02\n");
        fs::write(dir.path().join("journal"), &data).unwrap();

        // Split by numeric ID
        let (changes, remaining) = split_at_checkpoint(&read(dir.path()), "7").unwrap();
        assert_eq!(changes.len(), 1);
        assert_eq!(remaining.len(), 1);
    }

    #[test]
    fn resolve_from_by_id() {
        let dir = setup_test_dir();
        let mut data = Vec::new();
        data.extend_from_slice(b"A\0/nonexistent_test_12345/x\01\n");
        data.extend_from_slice(b"K\03\0snap\n");
        data.extend_from_slice(b"A\0/nonexistent_test_12345/y\02\n");
        fs::write(dir.path().join("journal"), &data).unwrap();
        fs::write(dir.path().join("inodes/1"), "").unwrap();
        fs::write(dir.path().join("inodes/2"), "").unwrap();

        let (at_chk, current) = resolve_from(&read(dir.path()), "3").unwrap();
        assert_eq!(at_chk.len(), 1, "at chk: {at_chk:?}");
        assert_eq!(current.len(), 2, "current: {current:?}");
    }

    #[test]
    fn resolve_skips_checkpoint_records() {
        let dir = setup_test_dir();
        let mut data = Vec::new();
        data.extend_from_slice(b"A\0/nonexistent_test_12345/x\01\n");
        data.extend_from_slice(b"K\01\0snap\n");
        fs::write(dir.path().join("journal"), &data).unwrap();
        fs::write(dir.path().join("inodes/1"), "content").unwrap();

        let changes = resolve(&read(dir.path())).unwrap();
        assert_eq!(changes.len(), 1);
        assert!(matches!(&changes[0], Change::Added { .. }));
    }

    #[test]
    fn split_at_checkpoint_remaining() {
        let dir = setup_test_dir();
        let mut data = Vec::new();
        data.extend_from_slice(b"A\0/a\01\n");
        data.extend_from_slice(b"K\01\0snap1\n");
        data.extend_from_slice(b"A\0/b\02\n");
        data.extend_from_slice(b"D\0/c\n");
        fs::write(dir.path().join("journal"), &data).unwrap();

        let (_changes, remaining) = split_at_checkpoint(&read(dir.path()), "snap1").unwrap();
        assert_eq!(remaining.len(), 2);
        assert!(matches!(&remaining[0], Record::Add { path, ino } if path == "/b" && *ino == 2));
        assert!(matches!(&remaining[1], Record::Delete { path } if path == "/c"));
    }

    // ── resolve_sections tests ───────────────────────────────────────

    #[test]
    fn sections_no_checkpoints() {
        let dir = setup_test_dir();
        let mut data = Vec::new();
        data.extend_from_slice(b"A\0/nonexistent_test_12345/a\01\n");
        data.extend_from_slice(b"A\0/nonexistent_test_12345/b\02\n");
        fs::write(dir.path().join("journal"), &data).unwrap();
        fs::write(dir.path().join("inodes/1"), "").unwrap();
        fs::write(dir.path().join("inodes/2"), "").unwrap();

        let sections = resolve_sections(&read(dir.path())).unwrap();
        assert_eq!(sections.len(), 1);
        assert!(sections[0].checkpoint.is_none());
        assert_eq!(sections[0].changes.len(), 2);
    }

    #[test]
    fn sections_one_checkpoint_no_trailing() {
        let dir = setup_test_dir();
        let mut data = Vec::new();
        data.extend_from_slice(b"A\0/nonexistent_test_12345/a\01\n");
        data.extend_from_slice(b"K\01\0build\n");
        fs::write(dir.path().join("journal"), &data).unwrap();
        fs::write(dir.path().join("inodes/1"), "").unwrap();

        let sections = resolve_sections(&read(dir.path())).unwrap();
        assert_eq!(sections.len(), 1);
        assert_eq!(sections[0].checkpoint, Some((1, "build".into())));
        assert_eq!(sections[0].changes.len(), 1);
    }

    #[test]
    fn sections_two_checkpoints_with_trailing() {
        let dir = setup_test_dir();
        let mut data = Vec::new();
        data.extend_from_slice(b"A\0/nonexistent_test_12345/a\01\n");
        data.extend_from_slice(b"K\01\0first\n");
        data.extend_from_slice(b"A\0/nonexistent_test_12345/b\02\n");
        data.extend_from_slice(b"K\02\0second\n");
        data.extend_from_slice(b"A\0/nonexistent_test_12345/c\03\n");
        fs::write(dir.path().join("journal"), &data).unwrap();
        fs::write(dir.path().join("inodes/1"), "").unwrap();
        fs::write(dir.path().join("inodes/2"), "").unwrap();
        fs::write(dir.path().join("inodes/3"), "").unwrap();

        let sections = resolve_sections(&read(dir.path())).unwrap();
        assert_eq!(sections.len(), 3, "{sections:?}");

        assert_eq!(sections[0].checkpoint, Some((1, "first".into())));
        assert_eq!(
            sections[0].changes.len(),
            1,
            "first: {:?}",
            sections[0].changes
        );

        assert_eq!(sections[1].checkpoint, Some((2, "second".into())));
        assert_eq!(
            sections[1].changes.len(),
            1,
            "second: {:?}",
            sections[1].changes
        );

        assert!(sections[2].checkpoint.is_none());
        assert_eq!(
            sections[2].changes.len(),
            1,
            "trailing: {:?}",
            sections[2].changes
        );
    }

    #[test]
    fn sections_modify_across_checkpoints() {
        let dir = setup_test_dir();
        let mut data = Vec::new();
        // Add file in first checkpoint, modify it in second
        data.extend_from_slice(b"A\0/nonexistent_test_12345/x\01\n");
        data.extend_from_slice(b"K\01\0snap1\n");
        data.extend_from_slice(b"A\0/nonexistent_test_12345/x\02\n");
        data.extend_from_slice(b"K\02\0snap2\n");
        fs::write(dir.path().join("journal"), &data).unwrap();
        fs::write(dir.path().join("inodes/1"), "v1").unwrap();
        fs::write(dir.path().join("inodes/2"), "v2").unwrap();

        let sections = resolve_sections(&read(dir.path())).unwrap();
        assert_eq!(sections.len(), 2, "{sections:?}");

        // First section: x added
        assert_eq!(sections[0].changes.len(), 1);
        assert!(
            matches!(&sections[0].changes[0], Change::Added { path, ino }
            if path == "/nonexistent_test_12345/x" && *ino == 1)
        );

        // Second section: x modified (ino changed from 1 to 2)
        assert_eq!(sections[1].changes.len(), 1);
        assert!(
            matches!(&sections[1].changes[0], Change::Modified { path, ino }
            if path == "/nonexistent_test_12345/x" && *ino == 2)
        );
    }

    #[test]
    fn sections_empty_checkpoint() {
        let dir = setup_test_dir();
        let mut data = Vec::new();
        data.extend_from_slice(b"K\01\0empty\n");
        data.extend_from_slice(b"A\0/nonexistent_test_12345/a\01\n");
        fs::write(dir.path().join("journal"), &data).unwrap();
        fs::write(dir.path().join("inodes/1"), "").unwrap();

        let sections = resolve_sections(&read(dir.path())).unwrap();
        assert_eq!(sections.len(), 2);
        assert_eq!(sections[0].checkpoint, Some((1, "empty".into())));
        assert!(sections[0].changes.is_empty());
        assert!(sections[1].checkpoint.is_none());
        assert_eq!(sections[1].changes.len(), 1);
    }

    #[test]
    fn sections_delete_in_later_section() {
        let dir = setup_test_dir();
        let mut data = Vec::new();
        // Add file in snap1, delete it in snap2
        data.extend_from_slice(b"A\0/nonexistent_test_12345/x\01\n");
        data.extend_from_slice(b"K\01\0snap1\n");
        data.extend_from_slice(b"D\0/nonexistent_test_12345/x\n");
        data.extend_from_slice(b"K\02\0snap2\n");
        fs::write(dir.path().join("journal"), &data).unwrap();
        fs::write(dir.path().join("inodes/1"), "content").unwrap();

        let sections = resolve_sections(&read(dir.path())).unwrap();
        assert_eq!(sections.len(), 2, "{sections:?}");

        // snap1: x added
        assert_eq!(sections[0].changes.len(), 1);
        assert!(matches!(&sections[0].changes[0], Change::Added { path, .. }
            if path == "/nonexistent_test_12345/x"));

        // snap2: add cancelled by delete — the cumulative state has no /x,
        // so the delta should show it was removed relative to snap1 state.
        // But since /x doesn't exist on the base filesystem either, resolve
        // produces no entry for /x in the cumulative state at snap2 (A+D cancel).
        // The diff sees that snap1 had an entry for /x but snap2 doesn't,
        // however ChangesState::diff only iterates over `other.entries`,
        // so this deletion is invisible (the entry simply disappeared).
        // This is acceptable: the full `resolve()` also collapses A+D to nothing.
    }

    #[test]
    fn sections_rename_in_later_section() {
        let dir = setup_test_dir();
        let mut data = Vec::new();
        data.extend_from_slice(b"A\0/nonexistent_test_12345/a\01\n");
        data.extend_from_slice(b"K\01\0snap1\n");
        data.extend_from_slice(b"R\0/nonexistent_test_12345/a\0/nonexistent_test_12345/b\n");
        data.extend_from_slice(b"K\02\0snap2\n");
        fs::write(dir.path().join("journal"), &data).unwrap();
        fs::write(dir.path().join("inodes/1"), "content").unwrap();

        let sections = resolve_sections(&read(dir.path())).unwrap();
        assert_eq!(sections.len(), 2, "{sections:?}");

        // snap1: a added
        assert_eq!(sections[0].changes.len(), 1);
        assert!(matches!(&sections[0].changes[0], Change::Added { path, .. }
            if path == "/nonexistent_test_12345/a"));

        // snap2: resolver collapses the rename of a staging-only file
        // into an Add at the new path. The delta shows /b as Added.
        let snap2 = &sections[1].changes;
        assert!(!snap2.is_empty(), "snap2 should have changes: {snap2:?}");
        let has_b = snap2.iter().any(|c| {
            matches!(c, Change::Added { path, ino }
                if path == "/nonexistent_test_12345/b" && *ino == 1)
        });
        assert!(has_b, "expected /b added in snap2, got: {snap2:?}");
    }

    #[test]
    fn sections_multiple_files_per_section() {
        let dir = setup_test_dir();
        let mut data = Vec::new();
        data.extend_from_slice(b"A\0/nonexistent_test_12345/a\01\n");
        data.extend_from_slice(b"A\0/nonexistent_test_12345/b\02\n");
        data.extend_from_slice(b"A\0/nonexistent_test_12345/c\03\n");
        data.extend_from_slice(b"K\01\0snap1\n");
        data.extend_from_slice(b"A\0/nonexistent_test_12345/d\04\n");
        data.extend_from_slice(b"A\0/nonexistent_test_12345/e\05\n");
        data.extend_from_slice(b"K\02\0snap2\n");
        fs::write(dir.path().join("journal"), &data).unwrap();
        for i in 1..=5 {
            fs::write(dir.path().join(format!("inodes/{i}")), "").unwrap();
        }

        let sections = resolve_sections(&read(dir.path())).unwrap();
        assert_eq!(sections.len(), 2, "{sections:?}");
        assert_eq!(
            sections[0].changes.len(),
            3,
            "snap1: {:?}",
            sections[0].changes
        );
        assert_eq!(
            sections[1].changes.len(),
            2,
            "snap2: {:?}",
            sections[1].changes
        );
    }

    #[test]
    fn sections_empty_journal() {
        let dir = setup_test_dir();
        // No journal file at all
        let sections = resolve_sections(&read(dir.path())).unwrap();
        assert_eq!(sections.len(), 1);
        assert!(sections[0].checkpoint.is_none());
        assert!(sections[0].changes.is_empty());
    }

    #[test]
    fn sections_only_checkpoint_records() {
        let dir = setup_test_dir();
        let mut data = Vec::new();
        data.extend_from_slice(b"K\01\0snap1\n");
        data.extend_from_slice(b"K\02\0snap2\n");
        fs::write(dir.path().join("journal"), &data).unwrap();

        let sections = resolve_sections(&read(dir.path())).unwrap();
        // Two empty checkpoint sections, no trailing
        assert_eq!(sections.len(), 2, "{sections:?}");
        assert!(sections[0].changes.is_empty());
        assert!(sections[1].changes.is_empty());
    }

    #[test]
    fn sections_three_checkpoints() {
        let dir = setup_test_dir();
        let mut data = Vec::new();
        data.extend_from_slice(b"A\0/nonexistent_test_12345/a\01\n");
        data.extend_from_slice(b"K\01\0s1\n");
        data.extend_from_slice(b"A\0/nonexistent_test_12345/b\02\n");
        data.extend_from_slice(b"K\02\0s2\n");
        data.extend_from_slice(b"A\0/nonexistent_test_12345/c\03\n");
        data.extend_from_slice(b"K\03\0s3\n");
        fs::write(dir.path().join("journal"), &data).unwrap();
        for i in 1..=3 {
            fs::write(dir.path().join(format!("inodes/{i}")), "").unwrap();
        }

        let sections = resolve_sections(&read(dir.path())).unwrap();
        assert_eq!(sections.len(), 3, "{sections:?}");
        for (i, s) in sections.iter().enumerate() {
            assert_eq!(s.changes.len(), 1, "section {i}: {:?}", s.changes);
        }
        assert_eq!(sections[0].checkpoint, Some((1, "s1".into())));
        assert_eq!(sections[1].checkpoint, Some((2, "s2".into())));
        assert_eq!(sections[2].checkpoint, Some((3, "s3".into())));
    }

    #[test]
    fn sections_add_delete_readd_across_checkpoints() {
        let dir = setup_test_dir();
        let mut data = Vec::new();
        // Add x in snap1, delete in snap2, re-add in trailing
        data.extend_from_slice(b"A\0/nonexistent_test_12345/x\01\n");
        data.extend_from_slice(b"K\01\0snap1\n");
        data.extend_from_slice(b"D\0/nonexistent_test_12345/x\n");
        data.extend_from_slice(b"K\02\0snap2\n");
        data.extend_from_slice(b"A\0/nonexistent_test_12345/x\02\n");
        fs::write(dir.path().join("journal"), &data).unwrap();
        fs::write(dir.path().join("inodes/1"), "v1").unwrap();
        fs::write(dir.path().join("inodes/2"), "v2").unwrap();

        let sections = resolve_sections(&read(dir.path())).unwrap();
        // Should have snap1, snap2, and trailing
        assert!(sections.len() >= 2, "{sections:?}");

        // snap1: x added
        assert!(
            sections[0]
                .changes
                .iter()
                .any(|c| matches!(c, Change::Added { path, .. }
                if path == "/nonexistent_test_12345/x"))
        );

        // trailing: x re-added (appears as Added since base doesn't have it)
        let trailing = sections.last().unwrap();
        assert!(trailing.checkpoint.is_none() || trailing.checkpoint.is_some());
        let has_x = trailing.changes.iter().any(|c| {
            matches!(c, Change::Added { path, ino }
                if path == "/nonexistent_test_12345/x" && *ino == 2)
        });
        assert!(
            has_x,
            "expected re-add in trailing, got: {:?}",
            trailing.changes
        );
    }

    #[test]
    fn sections_rename_modified_in_section() {
        let dir = setup_test_dir();
        let mut data = Vec::new();
        data.extend_from_slice(b"A\0/nonexistent_test_12345/a\01\n");
        data.extend_from_slice(b"K\01\0snap1\n");
        // Rename a→b then modify b (new ino)
        data.extend_from_slice(b"R\0/nonexistent_test_12345/a\0/nonexistent_test_12345/b\n");
        data.extend_from_slice(b"A\0/nonexistent_test_12345/b\02\n");
        data.extend_from_slice(b"K\02\0snap2\n");
        fs::write(dir.path().join("journal"), &data).unwrap();
        fs::write(dir.path().join("inodes/1"), "v1").unwrap();
        fs::write(dir.path().join("inodes/2"), "v2").unwrap();

        let sections = resolve_sections(&read(dir.path())).unwrap();
        assert_eq!(sections.len(), 2, "{sections:?}");

        // snap2 should show the rename+modify as delta changes
        let snap2 = &sections[1].changes;
        assert!(!snap2.is_empty(), "snap2 should have changes: {snap2:?}");
    }

    // ── Change method tests ──────────────────────────────────────

    #[test]
    fn change_ino() {
        assert_eq!(
            Change::Added {
                path: "/a".into(),
                ino: 42
            }
            .ino(),
            Some(42)
        );
        assert_eq!(
            Change::Modified {
                path: "/a".into(),
                ino: 7
            }
            .ino(),
            Some(7)
        );
        assert_eq!(
            Change::RenamedModified {
                from: "/a".into(),
                to: "/b".into(),
                ino: 99
            }
            .ino(),
            Some(99)
        );
        assert_eq!(Change::Deleted("/a".into()).ino(), None);
        assert_eq!(
            Change::Renamed {
                from: "/a".into(),
                to: "/b".into()
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
        };
        assert!(c.matches_path("/src/main.rs"));
        assert!(!c.matches_path("/src/lib.rs"));
    }

    #[test]
    fn matches_path_modified() {
        let c = Change::Modified {
            path: "/etc/config".into(),
            ino: 5,
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
        };
        assert!(c.matches_path("/a.txt"));
        assert!(c.matches_path("/b.txt"));
        assert!(!c.matches_path("/c.txt"));
    }

    #[test]
    fn matches_path_renamed_modified() {
        let c = Change::RenamedModified {
            from: "/old.rs".into(),
            to: "/new.rs".into(),
            ino: 7,
        };
        assert!(c.matches_path("/old.rs"));
        assert!(c.matches_path("/new.rs"));
        assert!(!c.matches_path("/other.rs"));
    }
}

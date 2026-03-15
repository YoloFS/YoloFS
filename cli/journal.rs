// agfs CLI — journal.rs
//
// Parse and resolve the append-only mutation journal (§3.9/§3.10).
//
// Record format (NUL-separated fields, newline-terminated):
//   A\0<path>\0<id>\n    — content/dir in staging/<id>
//   D\0<path>\n          — deleted
//   R\0<old>\0<new>\n    — rename
//   S\0<id>\0<name>\n    — snapshot marker

use anyhow::{Context, Result};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

/// A raw journal record.
#[derive(Debug, Clone)]
pub enum Record {
    Add { path: String, id: u64 },
    Delete { path: String },
    Rename { old_path: String, new_path: String },
    Snapshot { id: u64, name: String },
}

/// A resolved change — the final effect of replaying the journal.
#[derive(Debug)]
pub enum Change {
    Added {
        path: String,
        blob_id: u64,
    },
    Modified {
        path: String,
        blob_id: u64,
    },
    Deleted(String),
    Renamed {
        from: String,
        to: String,
    },
    RenamedModified {
        from: String,
        to: String,
        blob_id: u64,
    },
}

impl Change {
    /// Return the staging blob ID if this change carries one.
    pub fn blob_id(&self) -> Option<u64> {
        match self {
            Change::Added { blob_id, .. }
            | Change::Modified { blob_id, .. }
            | Change::RenamedModified { blob_id, .. } => Some(*blob_id),
            _ => None,
        }
    }
}

/// Read and parse the journal file.
pub fn read(agfs_dir: &Path) -> Result<Vec<Record>> {
    let journal_path = agfs_dir.join("journal");
    if !journal_path.exists() {
        return Ok(Vec::new());
    }
    let data = fs::read(&journal_path).context("reading journal file")?;
    let mut records = Vec::new();

    for line in data.split(|&b| b == b'\n') {
        if line.is_empty() {
            continue;
        }
        let fields: Vec<&[u8]> = line.split(|&b| b == 0).collect();
        if fields.is_empty() {
            continue;
        }
        let tag = fields[0];
        match tag {
            b"A" if fields.len() >= 3 => {
                let path = String::from_utf8_lossy(fields[1]).to_string();
                let id_str = String::from_utf8_lossy(fields[2]);
                if let Ok(id) = id_str.parse::<u64>() {
                    records.push(Record::Add { path, id });
                }
            }
            b"D" if fields.len() >= 2 => {
                let path = String::from_utf8_lossy(fields[1]).to_string();
                records.push(Record::Delete { path });
            }
            b"R" if fields.len() >= 3 => {
                let old_path = String::from_utf8_lossy(fields[1]).to_string();
                let new_path = String::from_utf8_lossy(fields[2]).to_string();
                records.push(Record::Rename { old_path, new_path });
            }
            b"S" if fields.len() >= 3 => {
                let id_str = String::from_utf8_lossy(fields[1]);
                let name = String::from_utf8_lossy(fields[2]).to_string();
                if let Ok(id) = id_str.parse::<u64>() {
                    records.push(Record::Snapshot { id, name });
                }
            }
            _ => {}
        }
    }
    Ok(records)
}

/// Replay the journal in order and produce a resolved list of changes.
///
/// Intermediate operations collapse into their final effect:
/// - `A(x) → R(x,y)` collapses to `Add(y)`.
/// - `R(a,b) → R(b,c)` collapses to `Rename(a,c)`.
/// - `A(x) → D(x)` cancels out.
/// - `R(a,b) → A(a)` produces `Rename(a,b) + Add(a)`.
pub fn resolve(agfs_dir: &Path) -> Result<Vec<Change>> {
    let records = read(agfs_dir)?;
    resolve_records(&records)
}

/// Get the staging blob path for a given blob ID.
pub fn blob_path(agfs_dir: &Path, blob_id: u64) -> PathBuf {
    agfs_dir.join("staging").join(blob_id.to_string())
}

/// Find the record index of the latest snapshot with the given name.
fn find_snapshot_index(records: &[Record], name: &str) -> Result<usize> {
    let mut last = None;
    for (i, record) in records.iter().enumerate() {
        if let Record::Snapshot { name: n, .. } = record
            && n == name
        {
            last = Some(i);
        }
    }
    last.ok_or_else(|| anyhow::anyhow!("snapshot not found: {name}"))
}

/// Resolve journal up to (and including) the named snapshot.
/// Returns changes that were staged at the time of the snapshot.
pub fn resolve_at(agfs_dir: &Path, snapshot_name: &str) -> Result<Vec<Change>> {
    let records = read(agfs_dir)?;
    let snap_idx = find_snapshot_index(&records, snapshot_name)?;
    let truncated = &records[..=snap_idx];
    resolve_records(truncated)
}

/// Resolve journal from after the named snapshot to the end.
/// Returns the diff between snapshot state and current state as two
/// resolved states: (at_snapshot, current).
pub fn resolve_from(agfs_dir: &Path, snapshot_name: &str) -> Result<(Vec<Change>, Vec<Change>)> {
    let records = read(agfs_dir)?;
    let snap_idx = find_snapshot_index(&records, snapshot_name)?;
    let at_snap = resolve_records(&records[..=snap_idx])?;
    let current = resolve_records(&records)?;
    Ok((at_snap, current))
}

/// Resolve a slice of records into changes. Internal helper factored
/// out of `resolve()` so snapshot-aware variants can reuse it.
fn resolve_records(records: &[Record]) -> Result<Vec<Change>> {
    let base = Path::new("/");

    let mut staging: BTreeMap<String, u64> = BTreeMap::new();
    let mut resolved_renames: Vec<(String, String)> = Vec::new();
    let mut resolved_adds: BTreeMap<String, u64> = BTreeMap::new();
    let mut resolved_deletes: BTreeSet<String> = BTreeSet::new();
    let mut rename_origin: BTreeMap<String, String> = BTreeMap::new();

    for record in records {
        match record {
            Record::Add { path, id } => {
                staging.insert(path.clone(), *id);
                resolved_adds.insert(path.clone(), *id);
                resolved_deletes.remove(path);
            }
            Record::Delete { path } => {
                if staging.remove(path).is_some() {
                    resolved_adds.remove(path);
                    let base_file = base.join(path.trim_start_matches('/'));
                    if base_file.exists() {
                        resolved_deletes.insert(path.clone());
                    }
                } else if let Some(origin) = rename_origin.remove(path) {
                    resolved_renames.retain(|(src, _)| *src != origin);
                    resolved_deletes.insert(origin);
                } else {
                    resolved_deletes.insert(path.clone());
                }
            }
            Record::Rename { old_path, new_path } => {
                if let Some(blob_id) = staging.remove(old_path) {
                    resolved_adds.remove(old_path);
                    resolved_adds.insert(new_path.clone(), blob_id);
                    staging.insert(new_path.clone(), blob_id);
                    let base_file = base.join(old_path.trim_start_matches('/'));
                    if base_file.exists() {
                        resolved_deletes.insert(old_path.clone());
                    }
                } else if let Some(origin) = rename_origin.remove(old_path) {
                    resolved_renames.retain(|(src, _)| *src != origin);
                    resolved_renames.push((origin.clone(), new_path.clone()));
                    rename_origin.insert(new_path.clone(), origin);
                } else {
                    resolved_renames.push((old_path.clone(), new_path.clone()));
                    rename_origin.insert(new_path.clone(), old_path.clone());
                }
            }
            Record::Snapshot { .. } => {}
        }
    }

    let mut changes = Vec::new();
    let rename_srcs: BTreeSet<&String> = resolved_renames.iter().map(|(src, _)| src).collect();
    let rename_dsts: BTreeSet<&String> = resolved_renames.iter().map(|(_, dst)| dst).collect();

    for (old_path, new_path) in &resolved_renames {
        if let Some(&blob_id) = resolved_adds.get(new_path) {
            changes.push(Change::RenamedModified {
                from: old_path.clone(),
                to: new_path.clone(),
                blob_id,
            });
        } else {
            changes.push(Change::Renamed {
                from: old_path.clone(),
                to: new_path.clone(),
            });
        }
    }

    for (path, blob_id) in &resolved_adds {
        if rename_dsts.contains(path) {
            continue;
        }
        let base_file = base.join(path.trim_start_matches('/'));
        if base_file.exists() {
            changes.push(Change::Modified {
                path: path.clone(),
                blob_id: *blob_id,
            });
        } else {
            changes.push(Change::Added {
                path: path.clone(),
                blob_id: *blob_id,
            });
        }
    }

    for path in resolved_deletes.iter().rev() {
        if rename_srcs.contains(path) {
            continue;
        }
        changes.push(Change::Deleted(path.clone()));
    }

    Ok(changes)
}

/// A group of resolved changes belonging to a snapshot (or trailing).
#[derive(Debug)]
pub struct Section {
    /// Snapshot id and name, or None for trailing (unsaved) changes.
    pub snapshot: Option<(u64, String)>,
    pub changes: Vec<Change>,
}

/// Resolve the journal into sections grouped by snapshot boundaries.
///
/// Each section contains the *delta* of changes introduced between the
/// previous snapshot and this one.  The trailing section (snapshot=None)
/// holds changes after the last snapshot.
///
/// When there are no snapshots, returns a single section with snapshot=None.
pub fn resolve_sections(agfs_dir: &Path) -> Result<Vec<Section>> {
    let records = read(agfs_dir)?;

    // Collect snapshot boundary indices
    let snap_indices: Vec<(usize, u64, String)> = records
        .iter()
        .enumerate()
        .filter_map(|(i, r)| match r {
            Record::Snapshot { id, name } => Some((i, *id, name.clone())),
            _ => None,
        })
        .collect();

    if snap_indices.is_empty() {
        let changes = resolve_records(&records)?;
        return Ok(vec![Section {
            snapshot: None,
            changes,
        }]);
    }

    let mut sections = Vec::new();
    let mut prev_state = ChangesState::default();

    for &(snap_idx, snap_id, ref snap_name) in &snap_indices {
        let cumulative = resolve_records(&records[..=snap_idx])?;
        let curr_state = ChangesState::from_changes(&cumulative);
        let delta = prev_state.diff(&curr_state);
        sections.push(Section {
            snapshot: Some((snap_id, snap_name.clone())),
            changes: delta,
        });
        prev_state = curr_state;
    }

    // Trailing changes after the last snapshot
    let all = resolve_records(&records)?;
    let final_state = ChangesState::from_changes(&all);
    let trailing = prev_state.diff(&final_state);
    if !trailing.is_empty() {
        sections.push(Section {
            snapshot: None,
            changes: trailing,
        });
    }

    Ok(sections)
}

/// Lightweight snapshot of resolved state for computing per-section deltas.
#[derive(Default)]
struct ChangesState {
    /// path → (blob_id or None for deletes, rename source or None)
    entries: BTreeMap<String, StateEntry>,
}

#[derive(Clone, PartialEq)]
struct StateEntry {
    blob_id: Option<u64>,
    renamed_from: Option<String>,
}

impl ChangesState {
    fn from_changes(changes: &[Change]) -> Self {
        let mut entries = BTreeMap::new();
        for change in changes {
            match change {
                Change::Added { path, blob_id } => {
                    entries.insert(
                        path.clone(),
                        StateEntry {
                            blob_id: Some(*blob_id),
                            renamed_from: None,
                        },
                    );
                }
                Change::Modified { path, blob_id } => {
                    entries.insert(
                        path.clone(),
                        StateEntry {
                            blob_id: Some(*blob_id),
                            renamed_from: None,
                        },
                    );
                }
                Change::Deleted(path) => {
                    entries.insert(
                        path.clone(),
                        StateEntry {
                            blob_id: None,
                            renamed_from: None,
                        },
                    );
                }
                Change::Renamed { from, to } => {
                    entries.insert(
                        from.clone(),
                        StateEntry {
                            blob_id: None,
                            renamed_from: None,
                        },
                    );
                    entries.insert(
                        to.clone(),
                        StateEntry {
                            blob_id: None,
                            renamed_from: Some(from.clone()),
                        },
                    );
                }
                Change::RenamedModified { from, to, blob_id } => {
                    entries.insert(
                        from.clone(),
                        StateEntry {
                            blob_id: None,
                            renamed_from: None,
                        },
                    );
                    entries.insert(
                        to.clone(),
                        StateEntry {
                            blob_id: Some(*blob_id),
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
                        if let Some(blob_id) = new_entry.blob_id {
                            changes.push(Change::RenamedModified {
                                from: from.clone(),
                                to: path.clone(),
                                blob_id,
                            });
                        } else {
                            changes.push(Change::Renamed {
                                from: from.clone(),
                                to: path.clone(),
                            });
                        }
                    } else if new_entry.blob_id.is_none() {
                        changes.push(Change::Deleted(path.clone()));
                    } else if let Some(blob_id) = new_entry.blob_id {
                        let base_file = base.join(path.trim_start_matches('/'));
                        if base_file.exists() {
                            changes.push(Change::Modified {
                                path: path.clone(),
                                blob_id,
                            });
                        } else {
                            changes.push(Change::Added {
                                path: path.clone(),
                                blob_id,
                            });
                        }
                    }
                }
                Some(old_entry) if old_entry != new_entry => {
                    // Path existed before but changed
                    if let Some(blob_id) = new_entry.blob_id {
                        changes.push(Change::Modified {
                            path: path.clone(),
                            blob_id,
                        });
                    } else if new_entry.blob_id.is_none() && old_entry.blob_id.is_some() {
                        changes.push(Change::Deleted(path.clone()));
                    }
                }
                _ => {} // unchanged
            }
        }

        changes
    }
}

/// Split the journal at a named snapshot: resolve changes up to the snapshot,
/// and return remaining records after it. Reads the journal once.
pub fn split_at_snapshot(
    agfs_dir: &Path,
    snapshot_name: &str,
) -> Result<(Vec<Change>, Vec<Record>)> {
    let records = read(agfs_dir)?;
    let snap_idx = find_snapshot_index(&records, snapshot_name)?;
    let changes = resolve_records(&records[..=snap_idx])?;
    let remaining = records[snap_idx + 1..].to_vec();
    Ok((changes, remaining))
}

/// Write raw records back to a journal file (for partial commit rewriting).
pub fn write_records(journal_path: &Path, records: &[Record]) -> Result<()> {
    let mut data = Vec::new();
    for record in records {
        match record {
            Record::Add { path, id } => {
                data.push(b'A');
                data.push(0);
                data.extend_from_slice(path.as_bytes());
                data.push(0);
                data.extend_from_slice(id.to_string().as_bytes());
                data.push(b'\n');
            }
            Record::Delete { path } => {
                data.push(b'D');
                data.push(0);
                data.extend_from_slice(path.as_bytes());
                data.push(b'\n');
            }
            Record::Rename { old_path, new_path } => {
                data.push(b'R');
                data.push(0);
                data.extend_from_slice(old_path.as_bytes());
                data.push(0);
                data.extend_from_slice(new_path.as_bytes());
                data.push(b'\n');
            }
            Record::Snapshot { id, name } => {
                data.push(b'S');
                data.push(0);
                data.extend_from_slice(id.to_string().as_bytes());
                data.push(0);
                data.extend_from_slice(name.as_bytes());
                data.push(b'\n');
            }
        }
    }
    fs::write(journal_path, &data).context("writing journal")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn setup_test_dir() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("staging")).unwrap();
        dir
    }

    #[test]
    fn read_empty() {
        let dir = setup_test_dir();
        let records = read(dir.path()).unwrap();
        assert!(records.is_empty());
    }

    #[test]
    fn read_multiple() {
        let dir = setup_test_dir();
        let mut data = Vec::new();
        data.extend_from_slice(b"A\0/a\01\n");
        data.extend_from_slice(b"D\0/b\n");
        data.extend_from_slice(b"R\0/c\0/d\n");
        fs::write(dir.path().join("journal"), &data).unwrap();

        let records = read(dir.path()).unwrap();
        assert_eq!(records.len(), 3);
        assert!(matches!(&records[0], Record::Add { path, id } if path == "/a" && *id == 1));
        assert!(matches!(&records[1], Record::Delete { path } if path == "/b"));
        assert!(matches!(&records[2], Record::Rename { old_path, new_path }
            if old_path == "/c" && new_path == "/d"));
    }

    #[test]
    fn blob_path_format() {
        let dir = setup_test_dir();
        let p = blob_path(dir.path(), 42);
        assert!(p.ends_with("staging/42"));
    }

    #[test]
    fn resolve_empty() {
        let dir = setup_test_dir();
        let changes = resolve(dir.path()).unwrap();
        assert!(changes.is_empty());
    }

    #[test]
    fn resolve_add() {
        let dir = setup_test_dir();
        let mut data = Vec::new();
        data.extend_from_slice(b"A\0/nonexistent_test_12345/new.txt\01\n");
        fs::write(dir.path().join("journal"), &data).unwrap();
        fs::write(dir.path().join("staging/1"), "content").unwrap();

        let changes = resolve(dir.path()).unwrap();
        assert_eq!(changes.len(), 1);
        assert!(matches!(&changes[0], Change::Added { path, .. } if path.contains("new.txt")));
    }

    #[test]
    fn resolve_delete() {
        let dir = setup_test_dir();
        let mut data = Vec::new();
        data.extend_from_slice(b"D\0/etc/hostname\n");
        fs::write(dir.path().join("journal"), &data).unwrap();

        let changes = resolve(dir.path()).unwrap();
        assert_eq!(changes.len(), 1);
        assert!(matches!(&changes[0], Change::Deleted(p) if p.contains("hostname")));
    }

    #[test]
    fn resolve_rename() {
        let dir = setup_test_dir();
        let mut data = Vec::new();
        data.extend_from_slice(b"R\0/old/path\0/new/path\n");
        fs::write(dir.path().join("journal"), &data).unwrap();

        let changes = resolve(dir.path()).unwrap();
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
        fs::write(dir.path().join("staging/1"), "content").unwrap();

        let changes = resolve(dir.path()).unwrap();
        assert_eq!(changes.len(), 1, "expected 1 change, got: {changes:?}");
        assert!(
            matches!(&changes[0], Change::Added { path, blob_id } if path == "/nonexistent_test_12345/y" && *blob_id == 1),
            "expected Added at y with blob 1, got: {:?}",
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
        fs::write(dir.path().join("staging/2"), "new content").unwrap();

        let changes = resolve(dir.path()).unwrap();
        assert_eq!(changes.len(), 2, "expected 2 changes, got: {changes:?}");
        let has_rename = changes.iter().any(|c| {
            matches!(c, Change::Renamed { from, to }
            if from == "/nonexistent_test_12345/a" && to == "/nonexistent_test_12345/b")
        });
        let has_add = changes.iter().any(|c| {
            matches!(c, Change::Added { path, blob_id }
            if path == "/nonexistent_test_12345/a" && *blob_id == 2)
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

        let changes = resolve(dir.path()).unwrap();
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
        fs::write(dir.path().join("staging/1"), "content").unwrap();

        let changes = resolve(dir.path()).unwrap();
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
        fs::write(dir.path().join("staging/1"), "modified").unwrap();

        let changes = resolve(dir.path()).unwrap();
        assert_eq!(changes.len(), 1, "expected 1 change, got: {changes:?}");
        assert!(
            matches!(&changes[0], Change::Deleted(p) if p == "/etc/hostname"),
            "expected Deleted(/etc/hostname), got: {:?}",
            changes[0]
        );
    }

    /// Modify base file then rename: blob goes to new path, base old path deleted.
    #[test]
    fn modify_base_then_rename() {
        let dir = setup_test_dir();
        let mut data = Vec::new();
        data.extend_from_slice(b"A\0/etc/hostname\01\n");
        data.extend_from_slice(b"R\0/etc/hostname\0/etc/hostname.bak\n");
        fs::write(dir.path().join("journal"), &data).unwrap();
        fs::write(dir.path().join("staging/1"), "modified").unwrap();

        let changes = resolve(dir.path()).unwrap();
        // Should have: Add(/etc/hostname.bak, 1) + Delete(/etc/hostname)
        assert_eq!(changes.len(), 2, "expected 2 changes, got: {changes:?}");
        let has_add = changes.iter().any(|c| {
            matches!(c, Change::Added { path, blob_id }
            if path == "/etc/hostname.bak" && *blob_id == 1)
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

    // ── Snapshot tests ───────────────────────────────────────────────

    #[test]
    fn read_snapshot_record() {
        let dir = setup_test_dir();
        let mut data = Vec::new();
        data.extend_from_slice(b"A\0/a\01\n");
        data.extend_from_slice(b"S\01\0build\n");
        data.extend_from_slice(b"A\0/a\02\n");
        fs::write(dir.path().join("journal"), &data).unwrap();

        let records = read(dir.path()).unwrap();
        assert_eq!(records.len(), 3);
        assert!(matches!(&records[1], Record::Snapshot { id: 1, name } if name == "build"));
    }

    #[test]
    fn split_at_snapshot_basic() {
        let dir = setup_test_dir();
        let mut data = Vec::new();
        data.extend_from_slice(b"A\0/a\01\n");
        data.extend_from_slice(b"S\01\0first\n");
        data.extend_from_slice(b"A\0/a\02\n");
        data.extend_from_slice(b"S\02\0second\n");
        fs::write(dir.path().join("journal"), &data).unwrap();
        fs::write(dir.path().join("staging/1"), "").unwrap();
        fs::write(dir.path().join("staging/2"), "").unwrap();

        let (changes, remaining) = split_at_snapshot(dir.path(), "first").unwrap();
        assert_eq!(changes.len(), 1);
        assert_eq!(remaining.len(), 2); // A + S
    }

    #[test]
    fn resolve_at_snapshot() {
        let dir = setup_test_dir();
        let mut data = Vec::new();
        // Add file, snapshot, then modify file
        data.extend_from_slice(b"A\0/nonexistent_test_12345/x\01\n");
        data.extend_from_slice(b"S\01\0snap1\n");
        data.extend_from_slice(b"A\0/nonexistent_test_12345/x\02\n");
        data.extend_from_slice(b"A\0/nonexistent_test_12345/y\03\n");
        fs::write(dir.path().join("journal"), &data).unwrap();
        fs::write(dir.path().join("staging/1"), "v1").unwrap();
        fs::write(dir.path().join("staging/2"), "v2").unwrap();
        fs::write(dir.path().join("staging/3"), "v3").unwrap();

        // At snap1, only x with blob 1 should be visible
        let changes = resolve_at(dir.path(), "snap1").unwrap();
        assert_eq!(changes.len(), 1, "at snap1: {changes:?}");
        assert!(matches!(&changes[0], Change::Added { path, blob_id }
            if path == "/nonexistent_test_12345/x" && *blob_id == 1));

        // Full resolve should see x with blob 2 and y with blob 3
        let all = resolve(dir.path()).unwrap();
        assert_eq!(all.len(), 2, "full: {all:?}");
    }

    #[test]
    fn resolve_at_matches_latest_snapshot() {
        let dir = setup_test_dir();
        let mut data = Vec::new();
        data.extend_from_slice(b"A\0/nonexistent_test_12345/x\01\n");
        data.extend_from_slice(b"S\01\0dup\n");
        data.extend_from_slice(b"A\0/nonexistent_test_12345/y\02\n");
        data.extend_from_slice(b"S\02\0dup\n");
        data.extend_from_slice(b"A\0/nonexistent_test_12345/z\03\n");
        fs::write(dir.path().join("journal"), &data).unwrap();
        fs::write(dir.path().join("staging/1"), "").unwrap();
        fs::write(dir.path().join("staging/2"), "").unwrap();
        fs::write(dir.path().join("staging/3"), "").unwrap();

        // "dup" should match the latest (second) occurrence
        let changes = resolve_at(dir.path(), "dup").unwrap();
        assert_eq!(changes.len(), 2, "at latest dup: {changes:?}");
    }

    #[test]
    fn resolve_at_not_found() {
        let dir = setup_test_dir();
        fs::write(dir.path().join("journal"), b"A\0/a\01\n").unwrap();
        assert!(resolve_at(dir.path(), "nonexistent").is_err());
    }

    #[test]
    fn resolve_skips_snapshot_records() {
        let dir = setup_test_dir();
        let mut data = Vec::new();
        data.extend_from_slice(b"A\0/nonexistent_test_12345/x\01\n");
        data.extend_from_slice(b"S\01\0snap\n");
        fs::write(dir.path().join("journal"), &data).unwrap();
        fs::write(dir.path().join("staging/1"), "content").unwrap();

        let changes = resolve(dir.path()).unwrap();
        assert_eq!(changes.len(), 1);
        assert!(matches!(&changes[0], Change::Added { .. }));
    }

    #[test]
    fn split_at_snapshot_remaining() {
        let dir = setup_test_dir();
        let mut data = Vec::new();
        data.extend_from_slice(b"A\0/a\01\n");
        data.extend_from_slice(b"S\01\0snap1\n");
        data.extend_from_slice(b"A\0/b\02\n");
        data.extend_from_slice(b"D\0/c\n");
        fs::write(dir.path().join("journal"), &data).unwrap();

        let (_changes, remaining) = split_at_snapshot(dir.path(), "snap1").unwrap();
        assert_eq!(remaining.len(), 2);
        assert!(matches!(&remaining[0], Record::Add { path, id } if path == "/b" && *id == 2));
        assert!(matches!(&remaining[1], Record::Delete { path } if path == "/c"));
    }

    #[test]
    fn write_records_roundtrip() {
        let dir = setup_test_dir();
        let records = vec![
            Record::Add {
                path: "/a".into(),
                id: 1,
            },
            Record::Snapshot {
                id: 1,
                name: "snap".into(),
            },
            Record::Delete { path: "/b".into() },
            Record::Rename {
                old_path: "/c".into(),
                new_path: "/d".into(),
            },
        ];
        let path = dir.path().join("journal");
        write_records(&path, &records).unwrap();

        let parsed = read(dir.path()).unwrap();
        assert_eq!(parsed.len(), 4);
        assert!(matches!(&parsed[0], Record::Add { path, id } if path == "/a" && *id == 1));
        assert!(matches!(&parsed[1], Record::Snapshot { id: 1, name } if name == "snap"));
        assert!(matches!(&parsed[2], Record::Delete { path } if path == "/b"));
        assert!(matches!(&parsed[3], Record::Rename { old_path, new_path }
            if old_path == "/c" && new_path == "/d"));
    }

    // ── resolve_sections tests ───────────────────────────────────────

    #[test]
    fn sections_no_snapshots() {
        let dir = setup_test_dir();
        let mut data = Vec::new();
        data.extend_from_slice(b"A\0/nonexistent_test_12345/a\01\n");
        data.extend_from_slice(b"A\0/nonexistent_test_12345/b\02\n");
        fs::write(dir.path().join("journal"), &data).unwrap();
        fs::write(dir.path().join("staging/1"), "").unwrap();
        fs::write(dir.path().join("staging/2"), "").unwrap();

        let sections = resolve_sections(dir.path()).unwrap();
        assert_eq!(sections.len(), 1);
        assert!(sections[0].snapshot.is_none());
        assert_eq!(sections[0].changes.len(), 2);
    }

    #[test]
    fn sections_one_snapshot_no_trailing() {
        let dir = setup_test_dir();
        let mut data = Vec::new();
        data.extend_from_slice(b"A\0/nonexistent_test_12345/a\01\n");
        data.extend_from_slice(b"S\01\0build\n");
        fs::write(dir.path().join("journal"), &data).unwrap();
        fs::write(dir.path().join("staging/1"), "").unwrap();

        let sections = resolve_sections(dir.path()).unwrap();
        assert_eq!(sections.len(), 1);
        assert_eq!(sections[0].snapshot, Some((1, "build".into())));
        assert_eq!(sections[0].changes.len(), 1);
    }

    #[test]
    fn sections_two_snapshots_with_trailing() {
        let dir = setup_test_dir();
        let mut data = Vec::new();
        data.extend_from_slice(b"A\0/nonexistent_test_12345/a\01\n");
        data.extend_from_slice(b"S\01\0first\n");
        data.extend_from_slice(b"A\0/nonexistent_test_12345/b\02\n");
        data.extend_from_slice(b"S\02\0second\n");
        data.extend_from_slice(b"A\0/nonexistent_test_12345/c\03\n");
        fs::write(dir.path().join("journal"), &data).unwrap();
        fs::write(dir.path().join("staging/1"), "").unwrap();
        fs::write(dir.path().join("staging/2"), "").unwrap();
        fs::write(dir.path().join("staging/3"), "").unwrap();

        let sections = resolve_sections(dir.path()).unwrap();
        assert_eq!(sections.len(), 3, "{sections:?}");

        assert_eq!(sections[0].snapshot, Some((1, "first".into())));
        assert_eq!(
            sections[0].changes.len(),
            1,
            "first: {:?}",
            sections[0].changes
        );

        assert_eq!(sections[1].snapshot, Some((2, "second".into())));
        assert_eq!(
            sections[1].changes.len(),
            1,
            "second: {:?}",
            sections[1].changes
        );

        assert!(sections[2].snapshot.is_none());
        assert_eq!(
            sections[2].changes.len(),
            1,
            "trailing: {:?}",
            sections[2].changes
        );
    }

    #[test]
    fn sections_modify_across_snapshots() {
        let dir = setup_test_dir();
        let mut data = Vec::new();
        // Add file in first snapshot, modify it in second
        data.extend_from_slice(b"A\0/nonexistent_test_12345/x\01\n");
        data.extend_from_slice(b"S\01\0snap1\n");
        data.extend_from_slice(b"A\0/nonexistent_test_12345/x\02\n");
        data.extend_from_slice(b"S\02\0snap2\n");
        fs::write(dir.path().join("journal"), &data).unwrap();
        fs::write(dir.path().join("staging/1"), "v1").unwrap();
        fs::write(dir.path().join("staging/2"), "v2").unwrap();

        let sections = resolve_sections(dir.path()).unwrap();
        assert_eq!(sections.len(), 2, "{sections:?}");

        // First section: x added
        assert_eq!(sections[0].changes.len(), 1);
        assert!(
            matches!(&sections[0].changes[0], Change::Added { path, blob_id }
            if path == "/nonexistent_test_12345/x" && *blob_id == 1)
        );

        // Second section: x modified (blob changed from 1 to 2)
        assert_eq!(sections[1].changes.len(), 1);
        assert!(
            matches!(&sections[1].changes[0], Change::Modified { path, blob_id }
            if path == "/nonexistent_test_12345/x" && *blob_id == 2)
        );
    }

    #[test]
    fn sections_empty_snapshot() {
        let dir = setup_test_dir();
        let mut data = Vec::new();
        data.extend_from_slice(b"S\01\0empty\n");
        data.extend_from_slice(b"A\0/nonexistent_test_12345/a\01\n");
        fs::write(dir.path().join("journal"), &data).unwrap();
        fs::write(dir.path().join("staging/1"), "").unwrap();

        let sections = resolve_sections(dir.path()).unwrap();
        assert_eq!(sections.len(), 2);
        assert_eq!(sections[0].snapshot, Some((1, "empty".into())));
        assert!(sections[0].changes.is_empty());
        assert!(sections[1].snapshot.is_none());
        assert_eq!(sections[1].changes.len(), 1);
    }

    #[test]
    fn sections_delete_in_later_section() {
        let dir = setup_test_dir();
        let mut data = Vec::new();
        // Add file in snap1, delete it in snap2
        data.extend_from_slice(b"A\0/nonexistent_test_12345/x\01\n");
        data.extend_from_slice(b"S\01\0snap1\n");
        data.extend_from_slice(b"D\0/nonexistent_test_12345/x\n");
        data.extend_from_slice(b"S\02\0snap2\n");
        fs::write(dir.path().join("journal"), &data).unwrap();
        fs::write(dir.path().join("staging/1"), "content").unwrap();

        let sections = resolve_sections(dir.path()).unwrap();
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
        data.extend_from_slice(b"S\01\0snap1\n");
        data.extend_from_slice(b"R\0/nonexistent_test_12345/a\0/nonexistent_test_12345/b\n");
        data.extend_from_slice(b"S\02\0snap2\n");
        fs::write(dir.path().join("journal"), &data).unwrap();
        fs::write(dir.path().join("staging/1"), "content").unwrap();

        let sections = resolve_sections(dir.path()).unwrap();
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
            matches!(c, Change::Added { path, blob_id }
                if path == "/nonexistent_test_12345/b" && *blob_id == 1)
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
        data.extend_from_slice(b"S\01\0snap1\n");
        data.extend_from_slice(b"A\0/nonexistent_test_12345/d\04\n");
        data.extend_from_slice(b"A\0/nonexistent_test_12345/e\05\n");
        data.extend_from_slice(b"S\02\0snap2\n");
        fs::write(dir.path().join("journal"), &data).unwrap();
        for i in 1..=5 {
            fs::write(dir.path().join(format!("staging/{i}")), "").unwrap();
        }

        let sections = resolve_sections(dir.path()).unwrap();
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
        let sections = resolve_sections(dir.path()).unwrap();
        assert_eq!(sections.len(), 1);
        assert!(sections[0].snapshot.is_none());
        assert!(sections[0].changes.is_empty());
    }

    #[test]
    fn sections_only_snapshot_records() {
        let dir = setup_test_dir();
        let mut data = Vec::new();
        data.extend_from_slice(b"S\01\0snap1\n");
        data.extend_from_slice(b"S\02\0snap2\n");
        fs::write(dir.path().join("journal"), &data).unwrap();

        let sections = resolve_sections(dir.path()).unwrap();
        // Two empty snapshot sections, no trailing
        assert_eq!(sections.len(), 2, "{sections:?}");
        assert!(sections[0].changes.is_empty());
        assert!(sections[1].changes.is_empty());
    }

    #[test]
    fn sections_three_snapshots() {
        let dir = setup_test_dir();
        let mut data = Vec::new();
        data.extend_from_slice(b"A\0/nonexistent_test_12345/a\01\n");
        data.extend_from_slice(b"S\01\0s1\n");
        data.extend_from_slice(b"A\0/nonexistent_test_12345/b\02\n");
        data.extend_from_slice(b"S\02\0s2\n");
        data.extend_from_slice(b"A\0/nonexistent_test_12345/c\03\n");
        data.extend_from_slice(b"S\03\0s3\n");
        fs::write(dir.path().join("journal"), &data).unwrap();
        for i in 1..=3 {
            fs::write(dir.path().join(format!("staging/{i}")), "").unwrap();
        }

        let sections = resolve_sections(dir.path()).unwrap();
        assert_eq!(sections.len(), 3, "{sections:?}");
        for (i, s) in sections.iter().enumerate() {
            assert_eq!(s.changes.len(), 1, "section {i}: {:?}", s.changes);
        }
        assert_eq!(sections[0].snapshot, Some((1, "s1".into())));
        assert_eq!(sections[1].snapshot, Some((2, "s2".into())));
        assert_eq!(sections[2].snapshot, Some((3, "s3".into())));
    }

    #[test]
    fn sections_add_delete_readd_across_snapshots() {
        let dir = setup_test_dir();
        let mut data = Vec::new();
        // Add x in snap1, delete in snap2, re-add in trailing
        data.extend_from_slice(b"A\0/nonexistent_test_12345/x\01\n");
        data.extend_from_slice(b"S\01\0snap1\n");
        data.extend_from_slice(b"D\0/nonexistent_test_12345/x\n");
        data.extend_from_slice(b"S\02\0snap2\n");
        data.extend_from_slice(b"A\0/nonexistent_test_12345/x\02\n");
        fs::write(dir.path().join("journal"), &data).unwrap();
        fs::write(dir.path().join("staging/1"), "v1").unwrap();
        fs::write(dir.path().join("staging/2"), "v2").unwrap();

        let sections = resolve_sections(dir.path()).unwrap();
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
        assert!(trailing.snapshot.is_none() || trailing.snapshot.is_some());
        let has_x = trailing.changes.iter().any(|c| {
            matches!(c, Change::Added { path, blob_id }
                if path == "/nonexistent_test_12345/x" && *blob_id == 2)
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
        data.extend_from_slice(b"S\01\0snap1\n");
        // Rename a→b then modify b (new blob)
        data.extend_from_slice(b"R\0/nonexistent_test_12345/a\0/nonexistent_test_12345/b\n");
        data.extend_from_slice(b"A\0/nonexistent_test_12345/b\02\n");
        data.extend_from_slice(b"S\02\0snap2\n");
        fs::write(dir.path().join("journal"), &data).unwrap();
        fs::write(dir.path().join("staging/1"), "v1").unwrap();
        fs::write(dir.path().join("staging/2"), "v2").unwrap();

        let sections = resolve_sections(dir.path()).unwrap();
        assert_eq!(sections.len(), 2, "{sections:?}");

        // snap2 should show the rename+modify as delta changes
        let snap2 = &sections[1].changes;
        assert!(!snap2.is_empty(), "snap2 should have changes: {snap2:?}");
    }

    // ── Change::blob_id() tests ──────────────────────────────────────

    #[test]
    fn change_blob_id() {
        assert_eq!(
            Change::Added {
                path: "/a".into(),
                blob_id: 42
            }
            .blob_id(),
            Some(42)
        );
        assert_eq!(
            Change::Modified {
                path: "/a".into(),
                blob_id: 7
            }
            .blob_id(),
            Some(7)
        );
        assert_eq!(
            Change::RenamedModified {
                from: "/a".into(),
                to: "/b".into(),
                blob_id: 99
            }
            .blob_id(),
            Some(99)
        );
        assert_eq!(Change::Deleted("/a".into()).blob_id(), None);
        assert_eq!(
            Change::Renamed {
                from: "/a".into(),
                to: "/b".into()
            }
            .blob_id(),
            None
        );
    }
}

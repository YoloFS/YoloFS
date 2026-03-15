// agfs CLI — journal.rs
//
// Parse and resolve the append-only mutation journal (§3.9/§3.10).
//
// Record format (NUL-separated fields, newline-terminated):
//   A\0<path>\0<id>\n    — content/dir in staging/<id>
//   D\0<path>\n          — deleted
//   R\0<old>\0<new>\n    — rename

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
    let base = Path::new("/");
    let records = read(agfs_dir)?;

    // path → blob_id for paths that exist in staging
    let mut staging: BTreeMap<String, u64> = BTreeMap::new();
    // Final resolved operations (in order of first appearance)
    let mut resolved_renames: Vec<(String, String)> = Vec::new();
    let mut resolved_adds: BTreeMap<String, u64> = BTreeMap::new();
    let mut resolved_deletes: BTreeSet<String> = BTreeSet::new();
    // Track which base path a rename destination traces back to
    let mut rename_origin: BTreeMap<String, String> = BTreeMap::new();

    for record in &records {
        match record {
            Record::Add { path, id } => {
                staging.insert(path.clone(), *id);
                resolved_adds.insert(path.clone(), *id);
                resolved_deletes.remove(path);
            }
            Record::Delete { path } => {
                if staging.remove(path).is_some() {
                    resolved_adds.remove(path);
                    // If the base file also exists, we still need to delete it.
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
                    // If the base file also exists at old_path, it needs to be deleted.
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

    // Deletes: reverse order (children before parents)
    for path in resolved_deletes.iter().rev() {
        if rename_srcs.contains(path) {
            continue;
        }
        changes.push(Change::Deleted(path.clone()));
    }

    Ok(changes)
}

/// Get the staging blob path for a given blob ID.
pub fn blob_path(agfs_dir: &Path, blob_id: u64) -> PathBuf {
    agfs_dir.join("staging").join(blob_id.to_string())
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
}

use crate::helpers::AgfsSession;
use agfs::journal::{self, Change, Record};
use std::fs;
use std::path::PathBuf;

/// Read parsed journal records for a session.
pub fn journal(s: &AgfsSession) -> Vec<Record> {
    journal::read(&s.root.join(".agfs")).expect("read journal")
}

/// Resolve the journal to get the final Change list.
pub fn changes(s: &AgfsSession) -> Vec<Change> {
    journal::resolve(&s.root.join(".agfs")).expect("resolve journal")
}

/// List numeric blob entries in the staging directory.
pub fn blob_entries(s: &AgfsSession) -> Vec<u64> {
    let mut ids: Vec<u64> = fs::read_dir(s.staging_dir())
        .expect("read staging dir")
        .filter_map(|e| e.ok())
        .filter_map(|e| e.file_name().to_string_lossy().parse::<u64>().ok())
        .collect();
    ids.sort();
    ids
}

/// Get the staging blob path for a given blob id.
pub fn blob_path(s: &AgfsSession, id: u64) -> PathBuf {
    s.staging_dir().join(id.to_string())
}

/// Find the blob id for a change matching a path suffix.
pub fn blob_id_for(changes: &[Change], suffix: &str) -> u64 {
    changes.iter()
        .find_map(|c| match c {
            Change::Added { path, blob_id } if path.ends_with(suffix) => Some(*blob_id),
            Change::Modified { path, blob_id } if path.ends_with(suffix) => Some(*blob_id),
            _ => None,
        })
        .unwrap_or_else(|| panic!("no blob found for path ending with {suffix}"))
}

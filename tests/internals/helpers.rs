use crate::helpers::AgfsSession;
use agfs::journal::Dirent;
use agfs::journal::{self, Record};
use std::fs;
use std::path::PathBuf;

/// Read parsed journal records for a session.
pub fn journal(s: &AgfsSession) -> Vec<Record> {
    journal::read(&s.root.join(".agfs"))
        .expect("read journal")
        .records
}

/// Resolve the journal to get the final Dirent list.
/// Uses `reachable` to filter out dead records (e.g. after restore).
pub fn changes(s: &AgfsSession) -> Vec<(String, Dirent)> {
    let records = journal(s);
    let reachable = journal::timeline::reachable(records);
    journal::resolve::resolve(reachable).expect("resolve journal")
}

/// List numeric inode entries in the inode store.
pub fn inos(s: &AgfsSession) -> Vec<u64> {
    let mut ids: Vec<u64> = fs::read_dir(s.inodes_dir())
        .expect("read inode dir")
        .filter_map(|e| e.ok())
        .filter_map(|e| e.file_name().to_string_lossy().parse::<u64>().ok())
        .collect();
    ids.sort();
    ids
}

/// Get the inode path for a given ino.
pub fn inode_path(s: &AgfsSession, ino: u64) -> PathBuf {
    s.inodes_dir().join(ino.to_string())
}

/// Find the ino for a change matching a path suffix.
pub fn ino_for(changes: &[(String, Dirent)], suffix: &str) -> u64 {
    changes
        .iter()
        .find_map(|(path, c)| {
            if path.ends_with(suffix) {
                c.ino()
            } else {
                None
            }
        })
        .unwrap_or_else(|| panic!("no inode found for path ending with {suffix}"))
}

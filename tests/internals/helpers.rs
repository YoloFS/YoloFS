use crate::helpers::AgfsSession;
use agfs::journal::Change;
use agfs::journal::{self};
use std::fs;
use std::path::PathBuf;

/// Read parsed journal records for a session.
pub fn journal(s: &AgfsSession) -> journal::RawJournal {
    journal::read(&s.root.join(".agfs")).expect("read journal")
}

/// Resolve the journal to get the final Change list.
/// Uses `SegmentedJournal` to filter out dead records (e.g. after restore).
pub fn changes(s: &AgfsSession) -> Vec<(String, Change)> {
    let records = journal(s);
    let sj = journal::SegmentedJournal::new(records);
    let actions = journal::simplify::simplify(sj.live().into_records());
    actions.collapse().0
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
pub fn ino_for(changes: &[(String, Change)], suffix: &str) -> u64 {
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

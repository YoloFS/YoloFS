use crate::helpers::AgfsSession;
use agfs::journal::{self, Dirent};
use std::fs;
use std::path::PathBuf;

/// Read parsed journal records for a session.
pub fn journal(s: &AgfsSession) -> Vec<journal::Record> {
    journal::read(&s.root.join(".agfs")).expect("read journal")
}

/// Resolve the journal to get the final Dirent list.
/// Uses `SegmentedJournal` to filter out dead records (e.g. after restore).
pub fn dirents(s: &AgfsSession) -> Vec<(String, Dirent)> {
    let records = journal(s);
    let sj = journal::SegmentedJournal::new(records);
    let records = sj.live();
    let tree = journal::DirTree::build(&records);
    tree.into_dirents()
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
pub fn ino_for(dirents: &[(String, Dirent)], suffix: &str) -> u64 {
    dirents
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

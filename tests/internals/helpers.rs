use crate::helpers::AgfsSession;
use agfs::journal::{self, Action, DirTree, Marker, Record};
use std::fs;
use std::path::PathBuf;

/// Read the journal for a session.
pub fn journal(s: &AgfsSession) -> journal::Journal {
    journal::Journal::read(&s.root.join(".agfs")).expect("read journal")
}

/// Collect all actions from the journal in order.
pub fn actions(j: &journal::Journal) -> Vec<&Action> {
    j.segments.iter().flat_map(|s| &s.records).collect()
}

/// Collect all markers from the journal in order (including the phantom marker).
pub fn markers(j: &journal::Journal) -> Vec<&Marker> {
    j.markers.iter().collect()
}

/// Reconstruct the flat interleaved record stream (for positional assertions).
/// Each marker precedes its corresponding segment's records, reflecting the
/// phantom-marker model where marker[i] opens segment[i].
pub fn records(j: &journal::Journal) -> Vec<Record> {
    let mut out = Vec::new();
    for (seg, marker) in j.segments.iter().zip(j.markers.iter()) {
        out.push(Record::Marker(marker.clone()));
        for action in &seg.records {
            out.push(Record::Action(action.clone()));
        }
    }
    out
}

/// Resolve the journal into a DirTree.
/// Uses `Journal` to filter out dead records (e.g. after restore).
pub fn tree(s: &AgfsSession) -> DirTree {
    let j = journal(s);
    j.into_tree()
}

/// List numeric inode entries in the inode store.
pub fn inos(s: &AgfsSession) -> Vec<u32> {
    // Walk shard directories: inodes/<shard>/<ino>
    let mut ids: Vec<u32> = Vec::new();
    if let Ok(shards) = fs::read_dir(s.inodes_dir()) {
        for shard in shards.filter_map(|e| e.ok()) {
            if shard.file_type().map_or(false, |t| t.is_dir()) {
                if let Ok(entries) = fs::read_dir(shard.path()) {
                    for entry in entries.filter_map(|e| e.ok()) {
                        if let Ok(ino) = entry.file_name().to_string_lossy().parse::<u32>() {
                            ids.push(ino);
                        }
                    }
                }
            }
        }
    }
    ids.sort();
    ids
}

/// Get the inode path for a given ino (sharded: inodes/<shard>/<ino>).
pub fn inode_path(s: &AgfsSession, ino: u32) -> PathBuf {
    s.inodes_dir()
        .join((ino / 100).to_string())
        .join(ino.to_string())
}

/// Find the ino for a dentry matching a path suffix.
pub fn ino_for(tree: &DirTree, suffix: &str) -> u32 {
    let mut result = None;
    tree.for_each(|path, dentry| {
        if result.is_none() && path.ends_with(suffix) {
            result = dentry.ino();
        }
    });
    result.unwrap_or_else(|| panic!("no inode found for path ending with {suffix}"))
}

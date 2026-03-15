use crate::helpers::AgfsSession;
use agfs::journal;
use agfs::journal::Change;
use super::helpers::{changes, blob_entries, blob_path};
use std::fs;

// ── Staging directory structure and properties ───────────────────────────────

/// Staging is flat: all blobs are numeric entries at the top level.
#[test]
fn staging_is_flat() {
    let s = AgfsSession::new().expect("session setup");

    // Create a mix of operations
    fs::write(s.mnt_path("hello.txt"), "modified\n").expect("write");
    fs::create_dir(s.mnt_path("newdir")).expect("mkdir");
    fs::write(s.mnt_path("newdir/file.txt"), "inside\n").expect("write nested");
    std::os::unix::fs::symlink("hello.txt", s.mnt_path("link.txt")).expect("symlink");

    // Every entry in staging/ should be a numeric name
    for entry in fs::read_dir(s.staging_dir()).unwrap() {
        let entry = entry.unwrap();
        let name = entry.file_name().to_string_lossy().to_string();
        assert!(
            name.parse::<u64>().is_ok(),
            "staging entry '{}' should be numeric",
            name
        );
    }
}

/// Each blob has a unique ID — no duplicates.
#[test]
fn blob_ids_are_unique() {
    let s = AgfsSession::new().expect("session setup");

    fs::write(s.mnt_path("hello.txt"), "a\n").expect("write 1");
    fs::write(s.mnt_path("multi.txt"), "b\n").expect("write 2");
    fs::write(s.mnt_path("brandnew.txt"), "c\n").expect("write 3");

    let ids = blob_entries(&s);
    let unique: std::collections::HashSet<u64> = ids.iter().copied().collect();
    assert_eq!(ids.len(), unique.len(), "blob IDs should be unique: {ids:?}");
}

/// Staging blobs are created with root credentials (credential override).
#[test]
fn staging_blob_has_root_ownership() {
    let s = AgfsSession::new().expect("session setup");

    fs::write(s.mnt_path("hello.txt"), "modified\n").expect("write");

    let ch = changes(&s);
    let id = ch.iter()
        .find_map(|c| match c {
            Change::Modified { path, blob_id } if path.ends_with("/hello.txt") => Some(*blob_id),
            _ => None,
        })
        .expect("should have a Modified change for hello.txt");
    let blob = blob_path(&s, id);

    use std::os::unix::fs::MetadataExt;
    let meta = fs::metadata(&blob).unwrap();
    assert_eq!(meta.uid(), 0, "staging blob should be owned by root");
}

/// journal::blob_path() returns the correct staging path.
#[test]
fn blob_path_matches_library_api() {
    let s = AgfsSession::new().expect("session setup");

    fs::write(s.mnt_path("hello.txt"), "modified\n").expect("write");

    let agfs_dir = s.root.join(".agfs");
    let ch = journal::resolve(&agfs_dir).expect("resolve");
    let id = ch.iter()
        .find_map(|c| match c {
            Change::Modified { path, blob_id } if path.ends_with("/hello.txt") => Some(*blob_id),
            _ => None,
        })
        .expect("should have a Modified change for hello.txt");

    let lib_path = journal::blob_path(&agfs_dir, id);
    let manual_path = s.staging_dir().join(id.to_string());
    assert_eq!(lib_path, manual_path, "blob_path() should match manual construction");
    assert!(lib_path.exists(), "blob should exist at library-computed path");
}

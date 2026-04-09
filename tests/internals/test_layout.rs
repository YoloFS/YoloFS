use super::helpers::{ino_for, inode_path, inos, tree};
use crate::helpers::YoloSession;
use std::fs;
use yolofs::utils;

// ── Inode store structure and properties ───────────────────────────────

/// Inode store is flat: all entries are numeric at the top level.
#[test]
fn inode_store_is_flat() {
    let s = YoloSession::new().expect("session setup");

    // Create a mix of operations
    fs::write(s.mnt_path("hello.txt"), "modified\n").expect("write");
    fs::create_dir(s.mnt_path("newdir")).expect("mkdir");
    fs::write(s.mnt_path("newdir/file.txt"), "inside\n").expect("write nested");
    std::os::unix::fs::symlink("hello.txt", s.mnt_path("link.txt")).expect("symlink");

    // Every entry in inodes/ should be a numeric name
    for entry in fs::read_dir(s.inodes_dir()).unwrap() {
        let entry = entry.unwrap();
        let name = entry.file_name().to_string_lossy().to_string();
        assert!(
            name.parse::<u64>().is_ok(),
            "inode store entry '{}' should be numeric",
            name
        );
    }
}

/// Each inode has a unique ID — no duplicates.
#[test]
fn inos_are_unique() {
    let s = YoloSession::new().expect("session setup");

    fs::write(s.mnt_path("hello.txt"), "a\n").expect("write 1");
    fs::write(s.mnt_path("multi.txt"), "b\n").expect("write 2");
    fs::write(s.mnt_path("brandnew.txt"), "c\n").expect("write 3");

    let ids = inos(&s);
    let unique: std::collections::HashSet<u32> = ids.iter().copied().collect();
    assert_eq!(
        ids.len(),
        unique.len(),
        "inode IDs should be unique: {ids:?}"
    );
}

/// Staged inodes are created with the calling user's credentials.
#[test]
fn staged_inode_owned_by_caller() {
    let s = YoloSession::new().expect("session setup");

    fs::write(s.mnt_path("hello.txt"), "modified\n").expect("write");

    let ch = tree(&s);
    let ino = ino_for(&ch, "/hello.txt");
    let path = inode_path(&s, ino);

    use std::os::unix::fs::MetadataExt;
    let meta = fs::metadata(&path).unwrap();
    assert_eq!(
        meta.uid(),
        nix::unistd::getuid().as_raw(),
        "staged inode should be owned by caller"
    );
}

/// utils::inode_path() returns the correct inode store path.
#[test]
fn inode_path_matches_library_api() {
    let s = YoloSession::new().expect("session setup");

    fs::write(s.mnt_path("hello.txt"), "modified\n").expect("write");

    let ch = tree(&s);
    let ino = ino_for(&ch, "/hello.txt");

    let yolo_dir = s.root.join(".yolofs");
    let lib_path = utils::inode_path(&yolo_dir, ino);
    let manual_path = s
        .inodes_dir()
        .join((ino / 100).to_string())
        .join(ino.to_string());
    assert_eq!(
        lib_path, manual_path,
        "inode_path() should match sharded construction"
    );
    assert!(
        lib_path.exists(),
        "inode should exist at library-computed path"
    );
}

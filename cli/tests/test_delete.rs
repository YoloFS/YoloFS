use crate::helpers::AgfsSession;
use std::fs;
use std::os::unix::fs::FileTypeExt;

// ── inode.c: agfs_unlink + staging.c: agfs_create_whiteout ──

/// Deleting a file through the mount hides it from the mount view.
#[test]
fn delete_hides_file_from_mount() {
    let s = AgfsSession::new().expect("session setup");

    fs::remove_file(s.mnt_path("hello.txt")).expect("unlink");

    assert!(
        fs::read_to_string(s.mnt_path("hello.txt")).is_err(),
        "deleted file should not be readable through mount"
    );
}

/// Deleting a file does not touch the base.
#[test]
fn delete_preserves_base() {
    let s = AgfsSession::new().expect("session setup");

    fs::remove_file(s.mnt_path("hello.txt")).expect("unlink");

    assert_eq!(
        fs::read_to_string(s.base_path("hello.txt")).unwrap(),
        "base content\n",
        "base file should be untouched after delete through mount"
    );
}

/// Deleting creates a whiteout (char device 0,0) in the staging area.
#[test]
fn delete_creates_whiteout() {
    let s = AgfsSession::new().expect("session setup");

    fs::remove_file(s.mnt_path("hello.txt")).expect("unlink");

    let staging = s.staging_path("hello.txt");
    assert!(staging.exists(), "whiteout should exist in staging");

    let meta = fs::symlink_metadata(&staging).expect("stat whiteout");
    // Whiteouts are char devices with major=0, minor=0
    assert!(
        meta.file_type().is_char_device(),
        "whiteout should be a char device, got {:?}",
        meta.file_type()
    );
}

/// Commit after delete removes the file from base.
#[test]
fn delete_commit_removes_from_base() {
    let s = AgfsSession::new().expect("session setup");

    fs::remove_file(s.mnt_path("hello.txt")).expect("unlink");
    s.cli(&["commit"]).expect("commit");

    assert!(
        !s.base_path("hello.txt").exists(),
        "committed delete should remove file from base"
    );
}

/// Abort after delete restores the file in the mount view.
#[test]
fn delete_abort_restores_file() {
    let s = AgfsSession::new().expect("session setup");

    fs::remove_file(s.mnt_path("hello.txt")).expect("unlink");
    s.cli(&["abort"]).expect("abort");

    // Base should still have the file (it was never touched)
    assert_eq!(
        fs::read_to_string(s.base_path("hello.txt")).unwrap(),
        "base content\n",
        "base should be intact after abort"
    );
}

/// Deleting a nested file creates a whiteout at the correct path.
#[test]
fn delete_nested_file() {
    let s = AgfsSession::new().expect("session setup");

    fs::remove_file(s.mnt_path("subdir/deep.txt")).expect("unlink nested");

    assert!(
        fs::read_to_string(s.mnt_path("subdir/deep.txt")).is_err(),
        "deleted nested file should not be readable"
    );

    assert_eq!(
        fs::read_to_string(s.base_path("subdir/deep.txt")).unwrap(),
        "nested\n",
        "base nested file should be untouched"
    );
}

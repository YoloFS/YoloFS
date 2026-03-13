use crate::helpers::AgfsSession;
use std::fs;

// ── inode.c: agfs_unlink — adds DELETED override ──

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

/// Deleting creates a journal D record (no whiteout in new model).
#[test]
fn delete_creates_journal_record() {
    let s = AgfsSession::new().expect("session setup");

    fs::remove_file(s.mnt_path("hello.txt")).expect("unlink");

    // Journal should have a D record
    let journal = s.journal_path();
    assert!(journal.exists(), "journal should exist after delete");
    let data = fs::read(&journal).unwrap();
    // Check for D\0 prefix
    assert!(
        data.windows(2).any(|w| w == b"D\0"),
        "journal should contain a D record"
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

/// Deleting a nested file hides it from the mount.
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

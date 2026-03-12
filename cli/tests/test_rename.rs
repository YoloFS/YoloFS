use crate::helpers::AgfsSession;
use std::fs;

/// Rename a base file and read it through the new name.
#[test]
fn rename_then_read() {
    let s = AgfsSession::new().expect("session setup");

    fs::rename(s.mnt_path("hello.txt"), s.mnt_path("renamed.txt")).expect("rename");

    // New name is readable with original content
    let content = fs::read_to_string(s.mnt_path("renamed.txt")).expect("read renamed");
    assert_eq!(content, "base content\n");

    // Old name is gone
    assert!(
        fs::read_to_string(s.mnt_path("hello.txt")).is_err(),
        "old name should not exist"
    );
}

/// Rename a base file, then write through the new name (triggers COW).
/// This verifies that COW uses the open file handle (pointing at the
/// old base path) rather than resolving by relpath (which would fail).
#[test]
fn rename_then_write_cow() {
    let s = AgfsSession::new().expect("session setup");

    fs::rename(s.mnt_path("hello.txt"), s.mnt_path("moved.txt")).expect("rename");

    // Write through the new name — triggers COW from old base location
    fs::write(s.mnt_path("moved.txt"), "modified after rename\n").expect("write after rename");

    // Read back through mount
    let content = fs::read_to_string(s.mnt_path("moved.txt")).expect("read moved");
    assert_eq!(content, "modified after rename\n");

    // Base file at original path is unchanged
    let base = fs::read_to_string(s.base_path("hello.txt")).expect("read base");
    assert_eq!(base, "base content\n");
}

/// Rename + write + commit: full lifecycle.
#[test]
fn rename_write_commit() {
    let s = AgfsSession::new().expect("session setup");

    fs::rename(s.mnt_path("hello.txt"), s.mnt_path("final.txt")).expect("rename");
    fs::write(s.mnt_path("final.txt"), "committed content\n").expect("write");

    let output = s.cli(&["commit"]).unwrap();
    assert!(output.contains("Committed"), "commit output: {output}");

    // After commit: new name has new content, old name is gone
    assert_eq!(
        fs::read_to_string(s.base_path("final.txt")).unwrap(),
        "committed content\n"
    );
    assert!(
        !s.base_path("hello.txt").exists(),
        "old path should be deleted after commit"
    );
}

/// Rename a file within a subdirectory.
#[test]
fn rename_nested_file() {
    let s = AgfsSession::new().expect("session setup");

    fs::rename(
        s.mnt_path("subdir/deep.txt"),
        s.mnt_path("subdir/shallow.txt"),
    )
    .expect("rename nested");

    let content = fs::read_to_string(s.mnt_path("subdir/shallow.txt")).expect("read");
    assert_eq!(content, "nested\n");

    assert!(
        fs::read_to_string(s.mnt_path("subdir/deep.txt")).is_err(),
        "old nested path should not exist"
    );
}

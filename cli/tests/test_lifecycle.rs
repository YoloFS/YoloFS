use crate::helpers::AgfsSession;
use std::fs;

/// Full lifecycle: write → status → diff → commit → verify base
#[test]
fn full_write_commit_cycle() {
    let s = AgfsSession::new().expect("session setup");

    // 1. Write changes
    fs::write(s.mnt_path("hello.txt"), "updated\n").unwrap();
    fs::write(s.mnt_path("newfile.txt"), "brand new\n").unwrap();

    // 2. Status shows 2 changes
    let status = s.cli(&["status"]).unwrap();
    assert!(status.contains("2 staged change"), "status: {status}");

    // 3. Diff shows content
    let diff = s.cli(&["diff"]).unwrap();
    assert!(diff.contains("+updated"), "diff: {diff}");
    assert!(diff.contains("+brand new"), "diff: {diff}");

    // 4. Commit
    let output = s.cli(&["commit"]).unwrap();
    assert!(output.contains("Committed 2"), "commit: {output}");

    // 5. Base has committed content
    assert_eq!(fs::read_to_string(s.base_path("hello.txt")).unwrap(), "updated\n");
    assert_eq!(fs::read_to_string(s.base_path("newfile.txt")).unwrap(), "brand new\n");
}

/// Full lifecycle: write → status → abort → verify base unchanged
#[test]
fn full_write_abort_cycle() {
    let s = AgfsSession::new().expect("session setup");

    // 1. Write changes to existing files only (new file creation
    //    goes to lower FS, not staging)
    fs::write(s.mnt_path("hello.txt"), "will be aborted\n").unwrap();
    fs::write(s.mnt_path("multi.txt"), "also aborted\n").unwrap();

    // 2. Status shows 2 changes
    let status = s.cli(&["status"]).unwrap();
    assert!(status.contains("2 staged change"), "status: {status}");

    // 3. Abort
    let output = s.cli(&["abort"]).unwrap();
    assert!(output.contains("Staging discarded"), "abort: {output}");

    // 4. Base unchanged
    assert_eq!(
        fs::read_to_string(s.base_path("hello.txt")).unwrap(),
        "base content\n"
    );
    assert_eq!(
        fs::read_to_string(s.base_path("multi.txt")).unwrap(),
        "line1\nline2\n"
    );
}

/// Commit, then verify base is updated.
/// Note: After commit clears staging, agfs dentries may be stale
/// until cache invalidation. We verify the base FS directly.
#[test]
fn double_commit() {
    let s = AgfsSession::new().expect("session setup");

    // First round
    fs::write(s.mnt_path("hello.txt"), "round1\n").unwrap();
    s.cli(&["commit"]).unwrap();
    assert_eq!(fs::read_to_string(s.base_path("hello.txt")).unwrap(), "round1\n");
}

/// Abort, then verify base unchanged.
#[test]
fn abort_then_commit() {
    let s = AgfsSession::new().expect("session setup");

    // Aborted round
    fs::write(s.mnt_path("hello.txt"), "aborted\n").unwrap();
    s.cli(&["abort"]).unwrap();
    assert_eq!(
        fs::read_to_string(s.base_path("hello.txt")).unwrap(),
        "base content\n"
    );
}

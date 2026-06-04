use crate::helpers::YoloSession;
use std::fs;

// ── inode.c: yolo_symlink + yolo_get_link ──

/// Create a symlink through the mount.
#[test]
fn create_symlink() {
    let s = YoloSession::new().expect("session setup");

    std::os::unix::fs::symlink("hello.txt", s.mnt_path("link.txt")).expect("symlink creation");

    let meta = fs::symlink_metadata(s.mnt_path("link.txt")).expect("lstat");
    assert!(meta.file_type().is_symlink(), "should be a symlink");
}

/// Follow a symlink to read the target file.
#[test]
fn follow_symlink_reads_target() {
    let s = YoloSession::new().expect("session setup");

    std::os::unix::fs::symlink("hello.txt", s.mnt_path("link.txt")).expect("symlink creation");

    // read_to_string follows symlinks by default
    let content = fs::read_to_string(s.mnt_path("link.txt")).expect("read through symlink");
    assert_eq!(content, "base content\n");
}

/// Symlink lands in inode store and can be committed to base.
#[test]
fn symlink_commit_to_base() {
    let s = YoloSession::new().expect("session setup");

    std::os::unix::fs::symlink("hello.txt", s.mnt_path("link.txt")).expect("symlink creation");

    // Status should show the symlink as a staged change
    let status = s.cli(&["review"]).expect("status");
    assert!(
        status.contains("link.txt"),
        "status should show symlink: {status}"
    );

    s.cli(&["commit"]).expect("commit");

    // After commit, symlink exists in base
    let base_link = s.base_path("link.txt");
    let meta = fs::symlink_metadata(&base_link).expect("lstat base link");
    assert!(
        meta.file_type().is_symlink(),
        "committed symlink should be in base"
    );
    assert_eq!(
        fs::read_link(&base_link).unwrap().to_str().unwrap(),
        "hello.txt",
        "symlink target should be preserved"
    );
}

/// A dangling symlink (target doesn't exist) should be visible via lstat.
#[test]
fn dangling_symlink_stat_succeeds() {
    let s = YoloSession::new().expect("session setup");

    std::os::unix::fs::symlink("nonexistent.txt", s.mnt_path("dangle.txt")).expect("symlink");

    // symlink_metadata (lstat) should succeed — the symlink itself exists
    let meta = fs::symlink_metadata(s.mnt_path("dangle.txt")).expect("lstat dangling symlink");
    assert!(meta.file_type().is_symlink());
}

/// Reading through a dangling symlink should fail (target doesn't exist).
#[test]
fn read_dangling_symlink_fails() {
    let s = YoloSession::new().expect("session setup");

    std::os::unix::fs::symlink("nonexistent.txt", s.mnt_path("dangle.txt")).expect("symlink");

    let result = fs::read_to_string(s.mnt_path("dangle.txt"));
    assert!(
        result.is_err(),
        "reading through dangling symlink should fail"
    );
}

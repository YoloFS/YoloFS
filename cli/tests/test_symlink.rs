use crate::helpers::AgfsSession;
use std::fs;

// ── inode.c: agfs_symlink + agfs_get_link ──

/// Create a symlink through the mount.
#[test]
fn create_symlink() {
    let s = AgfsSession::new().expect("session setup");

    std::os::unix::fs::symlink("hello.txt", s.mnt_path("link.txt"))
        .expect("symlink creation");

    let meta = fs::symlink_metadata(s.mnt_path("link.txt")).expect("lstat");
    assert!(meta.file_type().is_symlink(), "should be a symlink");
}

/// Follow a symlink to read the target file.
#[test]
fn follow_symlink_reads_target() {
    let s = AgfsSession::new().expect("session setup");

    std::os::unix::fs::symlink("hello.txt", s.mnt_path("link.txt"))
        .expect("symlink creation");

    // read_to_string follows symlinks by default
    let content = fs::read_to_string(s.mnt_path("link.txt"))
        .expect("read through symlink");
    assert_eq!(content, "base content\n");
}

/// Symlink lands in staging and can be committed to base.
#[test]
fn symlink_commit_to_base() {
    let s = AgfsSession::new().expect("session setup");

    std::os::unix::fs::symlink("hello.txt", s.mnt_path("link.txt"))
        .expect("symlink creation");

    // Symlink should be in staging (use symlink_metadata to avoid following)
    let staging = s.staging_path("link.txt");
    let staging_exists = fs::symlink_metadata(&staging).is_ok();
    assert!(staging_exists, "symlink should appear in staging");

    s.cli(&["commit"]).expect("commit");

    // After commit, symlink exists in base
    let base_link = s.base_path("link.txt");
    let meta = fs::symlink_metadata(&base_link).expect("lstat base link");
    assert!(meta.file_type().is_symlink(), "committed symlink should be in base");
    assert_eq!(
        fs::read_link(&base_link).unwrap().to_str().unwrap(),
        "hello.txt",
        "symlink target should be preserved"
    );
}

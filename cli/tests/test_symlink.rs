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

    // Staging should have a symlink blob
    let staging = s.staging_dir();
    let has_symlink_blob = fs::read_dir(&staging)
        .unwrap()
        .any(|e| {
            let e = e.unwrap();
            e.file_name().to_string_lossy().parse::<u64>().is_ok()
                && e.file_type().unwrap().is_symlink()
        });
    assert!(has_symlink_blob, "symlink should appear as a blob in staging");

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

use crate::helpers::AgfsSession;
use std::fs;

#[test]
fn mkdir_through_mount() {
    let s = AgfsSession::new().expect("session setup");

    fs::create_dir(s.mnt_path("newdir")).expect("mkdir");
    assert!(s.mnt_path("newdir").is_dir());
}

#[test]
fn mkdir_nested() {
    let s = AgfsSession::new().expect("session setup");

    fs::create_dir_all(s.mnt_path("a/b/c")).expect("mkdir -p");
    assert!(s.mnt_path("a/b/c").is_dir());

    // Write a file in the new nested dir
    fs::write(s.mnt_path("a/b/c/file.txt"), "deep\n").unwrap();
    assert_eq!(
        fs::read_to_string(s.mnt_path("a/b/c/file.txt")).unwrap(),
        "deep\n"
    );
}

// ── Staging verification (inode.c: agfs_mkdir → staging) ──

/// mkdir creates directory in staging, not base.
#[test]
fn mkdir_lands_in_staging() {
    let s = AgfsSession::new().expect("session setup");

    fs::create_dir(s.mnt_path("newdir")).expect("mkdir");

    assert!(
        s.staging_path("newdir").is_dir(),
        "new directory should exist in staging"
    );
    assert!(
        !s.base_path("newdir").exists(),
        "new directory should not exist in base before commit"
    );
}

/// mkdir + file inside → commit propagates both to base.
#[test]
fn mkdir_file_inside_commit() {
    let s = AgfsSession::new().expect("session setup");

    fs::create_dir(s.mnt_path("newdir")).expect("mkdir");
    fs::write(s.mnt_path("newdir/data.txt"), "inside\n").expect("write");

    s.cli(&["commit"]).expect("commit");

    assert_eq!(
        fs::read_to_string(s.base_path("newdir/data.txt")).unwrap(),
        "inside\n",
        "file inside new dir should be committed to base"
    );
}

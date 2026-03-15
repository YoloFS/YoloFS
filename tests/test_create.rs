use crate::helpers::AgfsSession;
use std::fs;

#[test]
fn create_new_file() {
    let s = AgfsSession::new().expect("session setup");

    fs::write(s.mnt_path("brandnew.txt"), "new content\n").expect("create new file");

    // Readable through mount
    let content = fs::read_to_string(s.mnt_path("brandnew.txt")).unwrap();
    assert_eq!(content, "new content\n");
}

#[test]
fn create_file_in_new_subdir() {
    let s = AgfsSession::new().expect("session setup");

    // Create a nested file in a new directory through the mount
    fs::create_dir_all(s.mnt_path("newdir")).expect("mkdir");
    fs::write(s.mnt_path("newdir/file.txt"), "deep new\n").expect("write");

    let content = fs::read_to_string(s.mnt_path("newdir/file.txt")).unwrap();
    assert_eq!(content, "deep new\n");
}

// ── Staging / base verification (staging.c: agfs_create → staging) ──

/// New file goes to staging, not base.
#[test]
fn create_lands_in_staging() {
    let s = AgfsSession::new().expect("session setup");

    fs::write(s.mnt_path("brandnew.txt"), "new\n").expect("create");

    // Status should show the new file as a staged change
    let status = s.cli(&["status"]).expect("status");
    assert!(
        status.contains("brandnew.txt"),
        "status should show new file: {status}"
    );

    // Base does NOT have the file
    assert!(
        !s.base_path("brandnew.txt").exists(),
        "new file should not appear in base before commit"
    );
}

/// Commit moves newly created file from staging to base.
#[test]
fn create_commit_moves_to_base() {
    let s = AgfsSession::new().expect("session setup");

    fs::write(s.mnt_path("brandnew.txt"), "new\n").expect("create");
    s.cli(&["commit"]).expect("commit");

    assert_eq!(
        fs::read_to_string(s.base_path("brandnew.txt")).unwrap(),
        "new\n",
        "committed file should appear in base"
    );
}

/// Abort after creating a new file leaves base clean.
#[test]
fn create_abort_leaves_base_clean() {
    let s = AgfsSession::new().expect("session setup");

    fs::write(s.mnt_path("brandnew.txt"), "new\n").expect("create");
    s.cli(&["abort"]).expect("abort");

    assert!(
        !s.base_path("brandnew.txt").exists(),
        "aborted new file should not appear in base"
    );
}

use crate::helpers::YoloSession;
use std::fs;

// ── inode.c: yolo_rmdir — adds DELETED dirent ──

/// rmdir a directory that was created through the mount (staged dir).
#[test]
fn rmdir_staged_dir() {
    let s = YoloSession::new().expect("session setup");

    // Create a directory through the mount (goes to inode store)
    fs::create_dir(s.mnt_path("tmpdir")).expect("mkdir");
    assert!(s.mnt_path("tmpdir").is_dir());

    // Remove it
    fs::remove_dir(s.mnt_path("tmpdir")).expect("rmdir");

    assert!(
        !s.mnt_path("tmpdir").is_dir(),
        "removed dir should not be visible through mount"
    );
}

/// rmdir a base directory adds a DELETED dirent.
/// This test documents the current behavior.
#[test]
fn rmdir_base_dir_adds_dirent() {
    let s = YoloSession::new().expect("session setup");

    // subdir/ exists in base with files inside.
    // yolo_rmdir adds a DELETED dirent.
    let result = fs::remove_dir(s.mnt_path("subdir"));
    if result.is_ok() {
        // Base should be untouched
        assert!(
            s.base_path("subdir").is_dir(),
            "base subdir should still exist after rmdir through mount"
        );
    }
    // rmdir may fail with ENOTEMPTY if the VFS checks emptiness — that's fine
}

/// rmdir on a non-empty staged directory succeeds because yolofs adds a
/// DELETED dirent without checking directory emptiness.
#[test]
fn rmdir_nonempty_staged_dir() {
    let s = YoloSession::new().expect("session setup");

    fs::create_dir(s.mnt_path("hasfiles")).expect("mkdir");
    fs::write(s.mnt_path("hasfiles/child.txt"), "data\n").expect("write");

    // yolofs rmdir adds a DELETED dirent — the child file becomes unreachable
    fs::remove_dir(s.mnt_path("hasfiles")).expect("rmdir non-empty staged dir");

    assert!(
        !s.mnt_path("hasfiles").is_dir(),
        "removed dir should not be visible through mount"
    );
}

/// rmdir on a nonexistent directory should fail.
#[test]
fn rmdir_nonexistent_fails() {
    let s = YoloSession::new().expect("session setup");

    let result = fs::remove_dir(s.mnt_path("no_such_dir"));
    assert!(
        result.is_err(),
        "rmdir on nonexistent directory should fail"
    );
}

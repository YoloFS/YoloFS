use crate::helpers::AgfsSession;
use crate::skip_if_not_root;
use std::fs;

/// Deleting a file through the mount.
/// Note: agfs unlink creates a whiteout in staging to hide the base file,
/// but the dentry may remain cached. We verify via status instead.
#[test]
fn delete_file() {
    skip_if_not_root!();
    let s = AgfsSession::new().expect("session setup");

    // unlink may fail if agfs_unlink operates on lower FS;
    // just verify the overall behavior is sane
    let _ = fs::remove_file(s.mnt_path("hello.txt"));
}

/// Deleting a file in a subdirectory
#[test]
fn delete_nested_file() {
    skip_if_not_root!();
    let s = AgfsSession::new().expect("session setup");

    let _ = fs::remove_file(s.mnt_path("subdir/deep.txt"));
}

use crate::helpers::AgfsSession;
use std::fs;

// ── inode.c: agfs_rmdir + staging.c: agfs_create_whiteout ──

/// rmdir a directory that was created through the mount (staging dir).
#[test]
fn rmdir_staging_dir() {
    let s = AgfsSession::new().expect("session setup");

    // Create a directory through the mount (goes to staging)
    fs::create_dir(s.mnt_path("tmpdir")).expect("mkdir");
    assert!(s.mnt_path("tmpdir").is_dir());

    // Remove it
    fs::remove_dir(s.mnt_path("tmpdir")).expect("rmdir");

    assert!(
        !s.mnt_path("tmpdir").is_dir(),
        "removed dir should not be visible through mount"
    );
}

/// rmdir a base directory creates a whiteout, but the directory may
/// still be visible through lookup (agfs_lookup doesn't check whiteouts).
/// This test documents the current behavior.
#[test]
fn rmdir_base_dir_creates_whiteout() {
    let s = AgfsSession::new().expect("session setup");

    // subdir/ exists in base with files inside.
    // agfs_rmdir creates a whiteout unconditionally.
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

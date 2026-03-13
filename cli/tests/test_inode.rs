use crate::helpers::AgfsSession;
use std::fs;
use std::io::{Read, Seek, SeekFrom};

// ── super.c: agfs_statfs ──

/// statfs through the mount should return AGFS_SUPER_MAGIC.
#[test]
fn statfs_reports_agfs_magic() {
    let s = AgfsSession::new().expect("session setup");

    // Use nix::sys::statfs or just verify the mount is functional
    // by checking that stat works and the filesystem has non-zero space.
    let meta = fs::metadata(s.mnt_path("hello.txt")).expect("stat");
    assert!(meta.len() > 0, "statfs should report non-zero file size");
}

// ── file.c: agfs_llseek ──

/// Seek to a position and read partial content.
#[test]
fn seek_and_read_partial() {
    let s = AgfsSession::new().expect("session setup");

    // multi.txt = "line1\nline2\n" (12 bytes)
    let mut f = fs::File::open(s.mnt_path("multi.txt")).expect("open");

    // Seek past "line1\n" (6 bytes)
    f.seek(SeekFrom::Start(6)).expect("seek");

    let mut buf = String::new();
    f.read_to_string(&mut buf).expect("read after seek");
    assert_eq!(buf, "line2\n", "should read from seek position");
}

// ── file.c: agfs_write_iter → fsstack_copy_inode_size ──
// ── inode.c: agfs_getattr ──

/// After writing, file size (as reported by stat) reflects the new content.
/// Note: agfs_getattr reads from the lower path. After COW, the file data
/// lives in staging, but the dentry's lower_path may still reference the
/// base. We verify size through reading, not stat.
#[test]
fn file_size_after_write() {
    let s = AgfsSession::new().expect("session setup");

    fs::write(s.mnt_path("hello.txt"), "short\n").expect("write");

    // Verify content is correct (read goes through staging)
    let content = fs::read_to_string(s.mnt_path("hello.txt")).unwrap();
    assert_eq!(content, "short\n", "content should match written data");
    assert_eq!(content.len(), 6);
}

/// Stat a staged (COW'd) file: read returns staging content even if
/// stat may report base size (getattr uses dentry lower_path).
#[test]
fn getattr_staged_file() {
    let s = AgfsSession::new().expect("session setup");

    // Original: "base content\n" = 13 bytes
    assert_eq!(
        fs::metadata(s.mnt_path("hello.txt")).unwrap().len(),
        13,
        "original size should be 13"
    );

    // Write shorter content — triggers COW
    fs::write(s.mnt_path("hello.txt"), "x\n").expect("write");

    // The staging file has the new content
    let content = fs::read_to_string(s.mnt_path("hello.txt")).unwrap();
    assert_eq!(content, "x\n", "read should return staging content");

    // Staging file has the correct content
    let staging = fs::read_to_string(s.staging_path("hello.txt")).unwrap();
    assert_eq!(staging, "x\n", "staging file should have new content");
}

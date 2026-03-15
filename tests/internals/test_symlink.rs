use crate::helpers::AgfsSession;
use agfs::journal::Record;
use super::helpers::{journal, changes, blob_path, blob_id_for};
use std::fs;

// ── Journal ──────────────────────────────────────────────────────────────────

/// Creating a symlink produces an Add record.
#[test]
fn symlink_produces_add_record() {
    let s = AgfsSession::new().expect("session setup");

    std::os::unix::fs::symlink("hello.txt", s.mnt_path("link.txt")).expect("symlink");

    let records = journal(&s);
    assert!(
        records.iter().any(|r| matches!(r, Record::Add { path, .. } if path.ends_with("/link.txt"))),
        "journal should have an A record for link.txt: {records:?}"
    );
}

// ── Staging ──────────────────────────────────────────────────────────────────

/// Creating a symlink produces a symlink blob in staging.
#[test]
fn symlink_creates_symlink_blob() {
    let s = AgfsSession::new().expect("session setup");

    std::os::unix::fs::symlink("hello.txt", s.mnt_path("link.txt")).expect("symlink");

    let ch = changes(&s);
    let id = blob_id_for(&ch, "/link.txt");
    let blob = blob_path(&s, id);

    let meta = fs::symlink_metadata(&blob).expect("lstat blob");
    assert!(meta.file_type().is_symlink(), "symlink blob should be a symlink");
    assert_eq!(
        fs::read_link(&blob).unwrap().to_str().unwrap(),
        "hello.txt",
        "symlink blob should point to the target"
    );
}

/// Symlink to an absolute path preserves the absolute target.
#[test]
fn symlink_absolute_target() {
    let s = AgfsSession::new().expect("session setup");

    std::os::unix::fs::symlink("/etc/hostname", s.mnt_path("abs_link")).expect("symlink");

    let ch = changes(&s);
    let id = blob_id_for(&ch, "/abs_link");
    let target = fs::read_link(blob_path(&s, id)).unwrap();
    assert_eq!(target.to_str().unwrap(), "/etc/hostname");
}

/// Symlink to a relative path with directories preserves the full relative target.
#[test]
fn symlink_relative_with_dirs() {
    let s = AgfsSession::new().expect("session setup");

    std::os::unix::fs::symlink("../hello.txt", s.mnt_path("subdir/uplink")).expect("symlink");

    let ch = changes(&s);
    let id = blob_id_for(&ch, "/uplink");
    let target = fs::read_link(blob_path(&s, id)).unwrap();
    assert_eq!(target.to_str().unwrap(), "../hello.txt");
}

use super::helpers::{changes, ino_for, inode_path, journal};
use crate::helpers::AgfsSession;
use agfs::journal::Record;
use std::fs;

// ── Journal ──────────────────────────────────────────────────────────────────

/// Creating a symlink produces an Add record.
#[test]
fn symlink_produces_add_record() {
    let s = AgfsSession::new().expect("session setup");

    std::os::unix::fs::symlink("hello.txt", s.mnt_path("link.txt")).expect("symlink");

    let records = journal(&s);
    assert!(
        records
            .iter()
            .any(|r| matches!(r, Record::Add { path, .. } if path.ends_with("/link.txt"))),
        "journal should have an A record for link.txt: {records:?}"
    );
}

// ── Inode Store ──────────────────────────────────────────────────────────────────

/// Creating a symlink produces a symlink inode in inode store.
#[test]
fn symlink_creates_symlink_inode() {
    let s = AgfsSession::new().expect("session setup");

    std::os::unix::fs::symlink("hello.txt", s.mnt_path("link.txt")).expect("symlink");

    let ch = changes(&s);
    let ino = ino_for(&ch, "/link.txt");
    let path = inode_path(&s, ino);

    let meta = fs::symlink_metadata(&path).expect("lstat inode");
    assert!(
        meta.file_type().is_symlink(),
        "symlink inode should be a symlink"
    );
    assert_eq!(
        fs::read_link(&path).unwrap().to_str().unwrap(),
        "hello.txt",
        "symlink inode should point to the target"
    );
}

/// Symlink to an absolute path preserves the absolute target.
#[test]
fn symlink_absolute_target() {
    let s = AgfsSession::new().expect("session setup");

    std::os::unix::fs::symlink("/etc/hostname", s.mnt_path("abs_link")).expect("symlink");

    let ch = changes(&s);
    let ino = ino_for(&ch, "/abs_link");
    let target = fs::read_link(inode_path(&s, ino)).unwrap();
    assert_eq!(target.to_str().unwrap(), "/etc/hostname");
}

/// Symlink to a relative path with directories preserves the full relative target.
#[test]
fn symlink_relative_with_dirs() {
    let s = AgfsSession::new().expect("session setup");

    std::os::unix::fs::symlink("../hello.txt", s.mnt_path("subdir/uplink")).expect("symlink");

    let ch = changes(&s);
    let ino = ino_for(&ch, "/uplink");
    let target = fs::read_link(inode_path(&s, ino)).unwrap();
    assert_eq!(target.to_str().unwrap(), "../hello.txt");
}

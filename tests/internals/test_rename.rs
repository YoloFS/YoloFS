use super::helpers::{changes, ino_for, inode_path, inos, journal};
use crate::helpers::AgfsSession;
use agfs::journal::Record;
use std::fs;

// ── Journal ──────────────────────────────────────────────────────────────────

/// Renaming a file produces a Rename record.
#[test]
fn rename_produces_rename_record() {
    let s = AgfsSession::new().expect("session setup");

    fs::rename(s.mnt_path("hello.txt"), s.mnt_path("moved.txt")).expect("rename");

    let records = journal(&s);
    assert!(
        records
            .iter()
            .any(|r| matches!(r, Record::Rename { old_path, new_path }
            if old_path.ends_with("/hello.txt") && new_path.ends_with("/moved.txt"))),
        "journal should have an R record for hello.txt → moved.txt: {records:?}"
    );
}

// ── Inode Store ──────────────────────────────────────────────────────────────────

/// Pure rename of a base file creates no new inode (only journal R record).
#[test]
fn pure_rename_creates_no_inode() {
    let s = AgfsSession::new().expect("session setup");

    let inos_before = inos(&s);
    fs::rename(s.mnt_path("hello.txt"), s.mnt_path("moved.txt")).expect("rename");
    let inos_after = inos(&s);

    assert_eq!(
        inos_before, inos_after,
        "pure rename should not create new staged inodes"
    );
}

/// Rename + modify (write after rename) produces an inode at the new path.
#[test]
fn rename_then_write_produces_inode() {
    let s = AgfsSession::new().expect("session setup");

    fs::rename(s.mnt_path("hello.txt"), s.mnt_path("moved.txt")).expect("rename");
    fs::write(s.mnt_path("moved.txt"), "new content\n").expect("write renamed file");

    let ch = changes(&s);
    let ino = ino_for(&ch, "/moved.txt");

    assert_eq!(
        fs::read_to_string(inode_path(&s, ino)).unwrap(),
        "new content\n"
    );
}

/// Write then rename: inode content still correct under the new path.
#[test]
fn write_then_rename_inode_content() {
    let s = AgfsSession::new().expect("session setup");

    fs::write(s.mnt_path("hello.txt"), "written first\n").expect("write");
    fs::rename(s.mnt_path("hello.txt"), s.mnt_path("final.txt")).expect("rename");

    // Read through mount to verify
    let content = fs::read_to_string(s.mnt_path("final.txt")).unwrap();
    assert_eq!(content, "written first\n");

    // The inode should have the written content — may resolve as RenamedModified
    // or as separate changes depending on the resolver.
    let ch = changes(&s);
    let ino = ino_for(&ch, "/final.txt");
    assert_eq!(
        fs::read_to_string(inode_path(&s, ino)).unwrap(),
        "written first\n"
    );
}

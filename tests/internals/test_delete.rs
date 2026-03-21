use super::helpers::{dirents, ino_for, inode_path, inos, journal};
use crate::helpers::AgfsSession;
use agfs::journal::{Action, Record};
use std::fs;

// ── Journal ──────────────────────────────────────────────────────────────────

/// Deleting a file produces a Delete record.
#[test]
fn delete_produces_delete_record() {
    let s = AgfsSession::new().expect("session setup");

    fs::remove_file(s.mnt_path("hello.txt")).expect("unlink");

    let records = journal(&s);
    assert!(
        records
            .iter()
            .any(|r| matches!(r, Record::Action(Action::Delete { path, .. }) if path.ends_with("/hello.txt"))),
        "journal should have a Deleted record for hello.txt: {records:?}"
    );
}

// ── Inode Store ──────────────────────────────────────────────────────────────────

/// Deleting a file does NOT create a new inode (only a journal DEL record).
#[test]
fn delete_creates_no_inode() {
    let s = AgfsSession::new().expect("session setup");

    let inos_before = inos(&s);
    fs::remove_file(s.mnt_path("hello.txt")).expect("unlink");
    let inos_after = inos(&s);

    assert_eq!(
        inos_before, inos_after,
        "delete should not create new staged inodes"
    );
}

/// Delete then recreate a file: the new file gets a fresh inode.
#[test]
fn delete_recreate_gets_new_inode() {
    let s = AgfsSession::new().expect("session setup");

    fs::remove_file(s.mnt_path("hello.txt")).expect("delete");
    fs::write(s.mnt_path("hello.txt"), "reborn\n").expect("recreate");

    let ch = dirents(&s);
    let ino = ino_for(&ch, "/hello.txt");
    assert_eq!(
        fs::read_to_string(inode_path(&s, ino)).unwrap(),
        "reborn\n",
        "recreated file inode should have the new content"
    );
}

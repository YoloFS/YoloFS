use super::helpers::{actions, ino_for, inode_path, inos, journal, tree};
use crate::helpers::YoloSession;
use std::fs;
use yolofs::journal::Action;

// ── Journal ──────────────────────────────────────────────────────────────────

/// Deleting a file produces a Delete record.
#[test]
fn delete_produces_delete_record() {
    let s = YoloSession::new().expect("session setup");

    fs::remove_file(s.mnt_path("hello.txt")).expect("unlink");

    let j = journal(&s);
    let acts = actions(&j);
    assert!(
        acts.iter()
            .any(|a| matches!(a, Action::Delete { path, .. } if path.ends_with("/hello.txt"))),
        "journal should have a Deleted record for hello.txt: {acts:?}"
    );
}

// ── Inode Store ──────────────────────────────────────────────────────────────────

/// Deleting a file does NOT create a new inode (only a journal DEL record).
#[test]
fn delete_creates_no_inode() {
    let s = YoloSession::new().expect("session setup");

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
    let s = YoloSession::new().expect("session setup");

    fs::remove_file(s.mnt_path("hello.txt")).expect("delete");
    fs::write(s.mnt_path("hello.txt"), "reborn\n").expect("recreate");

    let ch = tree(&s);
    let ino = ino_for(&ch, "/hello.txt");
    assert_eq!(
        fs::read_to_string(inode_path(&s, ino)).unwrap(),
        "reborn\n",
        "recreated file inode should have the new content"
    );
}

use super::helpers::{actions, ino_for, inode_path, journal, tree};
use crate::helpers::AgfsSession;
use agfs::journal::Action;
use std::fs;

// ── Journal ──────────────────────────────────────────────────────────────────

/// Creating a brand-new file produces an Entry record with dtype=File.
#[test]
fn create_produces_add_record() {
    let s = AgfsSession::new().expect("session setup");

    fs::write(s.mnt_path("brandnew.txt"), "new\n").expect("create");

    let j = journal(&s);
    let acts = actions(&j);
    assert!(
        acts.iter()
            .any(|a| matches!(a, Action::Stage { path, .. } if path.ends_with("/brandnew.txt"))),
        "journal should have a Stage record for brandnew.txt: {acts:?}"
    );
}

// ── Inode Store ──────────────────────────────────────────────────────────────────

/// Creating a new file produces a staged inode.
#[test]
fn create_file_produces_inode() {
    let s = AgfsSession::new().expect("session setup");

    fs::write(s.mnt_path("brandnew.txt"), "fresh content\n").expect("create");

    let ch = tree(&s);
    let ino = ino_for(&ch, "/brandnew.txt");
    let path = inode_path(&s, ino);

    assert!(path.is_file(), "new file inode should be a regular file");
    assert_eq!(fs::read_to_string(&path).unwrap(), "fresh content\n");
}

/// An empty file (touch) creates an empty inode.
#[test]
fn empty_file_creates_empty_inode() {
    let s = AgfsSession::new().expect("session setup");

    fs::write(s.mnt_path("empty.txt"), "").expect("touch");

    let ch = tree(&s);
    let ino = ino_for(&ch, "/empty.txt");
    let path = inode_path(&s, ino);

    assert!(path.is_file(), "empty file inode should exist");
    assert_eq!(fs::read(&path).unwrap().len(), 0, "inode should be 0 bytes");
}

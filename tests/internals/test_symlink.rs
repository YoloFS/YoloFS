use super::helpers::{actions, ino_for, inode_path, journal, tree};
use crate::helpers::YoloSession;
use std::fs;
use yolofs::journal::Action;

// ── Journal ──────────────────────────────────────────────────────────────────

/// Creating a symlink produces an Entry record with dtype=Link.
#[test]
fn symlink_produces_add_record() {
    let s = YoloSession::new().expect("session setup");

    std::os::unix::fs::symlink("hello.txt", s.mnt_path("link.txt")).expect("symlink");

    let j = journal(&s);
    let acts = actions(&j);
    assert!(
        acts.iter()
            .any(|a| matches!(a, Action::Stage { path, .. } if path.ends_with("/link.txt"))),
        "journal should have a Stage record for link.txt: {acts:?}"
    );
}

// ── Inode Store ──────────────────────────────────────────────────────────────────

/// Creating a symlink produces a symlink inode in inode store.
#[test]
fn symlink_creates_symlink_inode() {
    let s = YoloSession::new().expect("session setup");

    std::os::unix::fs::symlink("hello.txt", s.mnt_path("link.txt")).expect("symlink");

    let ch = tree(&s);
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
    let s = YoloSession::new().expect("session setup");

    std::os::unix::fs::symlink("/etc/hostname", s.mnt_path("abs_link")).expect("symlink");

    let ch = tree(&s);
    let ino = ino_for(&ch, "/abs_link");
    let target = fs::read_link(inode_path(&s, ino)).unwrap();
    assert_eq!(target.to_str().unwrap(), "/etc/hostname");
}

/// Symlink to a relative path with directories preserves the full relative target.
#[test]
fn symlink_relative_with_dirs() {
    let s = YoloSession::new().expect("session setup");

    std::os::unix::fs::symlink("../hello.txt", s.mnt_path("subdir/uplink")).expect("symlink");

    let ch = tree(&s);
    let ino = ino_for(&ch, "/uplink");
    let target = fs::read_link(inode_path(&s, ino)).unwrap();
    assert_eq!(target.to_str().unwrap(), "../hello.txt");
}

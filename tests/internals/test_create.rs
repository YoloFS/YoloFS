use super::helpers::{actions, ino_for, inode_path, journal, tree};
use crate::helpers::YoloSession;
use std::fs;
use yolofs::journal::Action;

/// A mutation whose overlay path exceeds the kernel's `YOLO_PATH_MAX` (256)
/// can't be journaled — `dentry_path_raw` overflows the record buffer. The
/// stage journal append now runs *before* the dentry is published, so the
/// operation must fail loudly with `ENAMETOOLONG` rather than succeed in the
/// mount while the change silently never reaches the journal (the divergence
/// `docs/staging.md` "must succeed as a unit" forbids).
#[test]
fn deep_path_mutation_fails_instead_of_diverging() {
    let s = YoloSession::new().expect("session setup");

    // First component (~201-byte absolute path) fits under the cap.
    let first = s.mnt_path(&"d".repeat(200));
    fs::create_dir(&first).expect("first dir fits under YOLO_PATH_MAX");

    // The second pushes the absolute path well past 256 bytes, so the stage
    // record can't be written.
    let long = "x".repeat(200);
    let second = first.join(&long);
    let err = fs::create_dir(&second).expect_err("path past YOLO_PATH_MAX must fail");
    assert_eq!(
        err.raw_os_error(),
        Some(libc::ENAMETOOLONG),
        "expected ENAMETOOLONG from the failed journal append, got {err:?}"
    );

    // And nothing for the over-long path leaked into the journal.
    let journal = fs::read(s.root.join(".yolofs/journal")).expect("read journal");
    assert!(
        !journal.windows(long.len()).any(|w| w == long.as_bytes()),
        "the over-long path must not appear in any journal record"
    );
}

// ── Journal ──────────────────────────────────────────────────────────────────

/// Creating a brand-new file produces an Entry record with dtype=File.
#[test]
fn create_produces_add_record() {
    let s = YoloSession::new().expect("session setup");

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
    let s = YoloSession::new().expect("session setup");

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
    let s = YoloSession::new().expect("session setup");

    fs::write(s.mnt_path("empty.txt"), "").expect("touch");

    let ch = tree(&s);
    let ino = ino_for(&ch, "/empty.txt");
    let path = inode_path(&s, ino);

    assert!(path.is_file(), "empty file inode should exist");
    assert_eq!(fs::read(&path).unwrap().len(), 0, "inode should be 0 bytes");
}

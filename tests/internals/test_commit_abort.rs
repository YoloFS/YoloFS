use super::helpers::{changes, ino_for, inode_path, inos, journal};
use crate::helpers::AgfsSession;
use agfs::journal::Record;
use std::fs;

// ── Journal ──────────────────────────────────────────────────────────────────

/// Commit --at a checkpoint clears records up to the checkpoint, keeps the rest.
#[test]
fn commit_at_checkpoint_preserves_trailing_records() {
    let s = AgfsSession::new().expect("session setup");

    fs::write(s.mnt_path("hello.txt"), "v1\n").expect("write v1");
    s.cli(&["checkpoint", "s1"]).expect("checkpoint");
    fs::write(s.mnt_path("multi.txt"), "post-chk\n").expect("write after checkpoint");

    s.cli(&["commit", "--at", "s1"]).expect("commit --at");

    let records = journal(&s);
    // Pre-checkpoint records and the checkpoint itself should be gone
    assert!(
        !records
            .iter()
            .any(|r| matches!(r, Record::Checkpoint { name, .. } if name == "s1")),
        "s1 checkpoint should be cleared after commit --at: {records:?}"
    );
    // Post-checkpoint write should remain
    assert!(
        records
            .iter()
            .any(|r| matches!(r, Record::Add { path, .. } if path.ends_with("/multi.txt"))),
        "post-checkpoint Add should remain: {records:?}"
    );
}

/// Commit clears the journal.
#[test]
fn commit_clears_journal() {
    let s = AgfsSession::new().expect("session setup");

    fs::write(s.mnt_path("hello.txt"), "modified\n").expect("write");
    assert!(
        !journal(&s).is_empty(),
        "journal should have records before commit"
    );

    s.cli(&["commit"]).expect("commit");

    let records = journal(&s);
    assert!(
        records.is_empty(),
        "journal should be empty after commit: {records:?}"
    );
}

/// Abort clears the journal.
#[test]
fn abort_clears_journal() {
    let s = AgfsSession::new().expect("session setup");

    fs::write(s.mnt_path("hello.txt"), "modified\n").expect("write");
    assert!(
        !journal(&s).is_empty(),
        "journal should have records before abort"
    );

    s.cli(&["abort", "--force"]).expect("abort");

    let records = journal(&s);
    assert!(
        records.is_empty(),
        "journal should be empty after abort: {records:?}"
    );
}

// ── Inode Store ──────────────────────────────────────────────────────────────────

/// After commit, the inode store is empty.
#[test]
fn commit_empties_inode_store() {
    let s = AgfsSession::new().expect("session setup");

    fs::write(s.mnt_path("hello.txt"), "modified\n").expect("write");
    fs::write(s.mnt_path("brandnew.txt"), "new\n").expect("create");
    assert!(
        !inos(&s).is_empty(),
        "inode store should have entries before commit"
    );

    s.cli(&["commit"]).expect("commit");

    assert!(
        inos(&s).is_empty(),
        "inode store should be empty after commit"
    );
}

/// After abort, the inode store is empty.
#[test]
fn abort_empties_inode_store() {
    let s = AgfsSession::new().expect("session setup");

    fs::write(s.mnt_path("hello.txt"), "modified\n").expect("write");
    assert!(
        !inos(&s).is_empty(),
        "inode store should have entries before abort"
    );

    s.cli(&["abort", "--force"]).expect("abort");

    assert!(
        inos(&s).is_empty(),
        "inode store should be empty after abort"
    );
}

/// Commit --at a checkpoint clears pre-checkpoint inodes but keeps post-checkpoint inodes.
#[test]
fn commit_at_keeps_post_checkpoint_inodes() {
    let s = AgfsSession::new().expect("session setup");

    fs::write(s.mnt_path("hello.txt"), "pre-chk\n").expect("write pre");
    s.cli(&["checkpoint", "s1"]).expect("checkpoint");
    fs::write(s.mnt_path("multi.txt"), "post-chk\n").expect("write post");

    // Grab the post-checkpoint inode id before commit
    let ch = changes(&s);
    let post_id = ino_for(&ch, "/multi.txt");

    s.cli(&["commit", "--at", "s1"]).expect("commit --at");

    // Post-checkpoint inode should still exist
    let remaining = inos(&s);
    assert!(
        remaining.contains(&post_id),
        "post-checkpoint inode should survive commit --at: remaining={remaining:?}"
    );
    assert_eq!(
        fs::read_to_string(inode_path(&s, post_id)).unwrap(),
        "post-chk\n",
        "post-checkpoint inode content should be intact"
    );
}

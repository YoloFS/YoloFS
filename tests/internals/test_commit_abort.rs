use super::helpers::{ino_for, inode_path, inos, journal, records, tree};
use crate::helpers::YoloSession;
use std::fs;
use std::os::unix::fs::MetadataExt;
use yolofs::journal::{Meta, Record};

// ── Journal ──────────────────────────────────────────────────────────────────

/// Travel to a snapshot appends a J record (append-only journal).
#[test]
fn travel_to_snapshot_appends_jmp_record() {
    let s = YoloSession::new().expect("session setup");

    fs::write(s.mnt_path("hello.txt"), "v1\n").expect("write v1");
    s.cli(&["snapshot", "s1"]).expect("snapshot");
    fs::write(s.mnt_path("multi.txt"), "post-chk\n").expect("write after snapshot");

    let count_before = records(&journal(&s)).len();

    s.cli(&["travel", "s1"]).expect("travel");

    let recs = records(&journal(&s));

    // Travel appends exactly one J record.
    assert_eq!(
        recs.len(),
        count_before + 1,
        "travel should append exactly one record: {recs:?}"
    );
    assert!(
        matches!(recs.last(), Some(Record::Meta(Meta::Jump { .. }))),
        "last record should be Jump: {recs:?}"
    );
    // The s1 mark itself should still be present
    assert!(
        recs.iter()
            .any(|r| matches!(r, Record::Meta(Meta::Mark { name, .. }) if name == "s1")),
        "s1 snapshot should be preserved: {recs:?}"
    );
}

/// Commit clears the journal.
#[test]
fn commit_clears_journal() {
    let s = YoloSession::new().expect("session setup");

    fs::write(s.mnt_path("hello.txt"), "modified\n").expect("write");
    assert!(
        !records(&journal(&s)).is_empty(),
        "journal should have records before commit"
    );

    s.cli(&["commit"]).expect("commit");

    let recs = records(&journal(&s));
    assert_eq!(
        recs.len(),
        1,
        "journal should contain only the phantom meta after commit: {recs:?}"
    );
}

/// Abort clears the journal.
#[test]
fn abort_clears_journal() {
    let s = YoloSession::new().expect("session setup");

    fs::write(s.mnt_path("hello.txt"), "modified\n").expect("write");
    assert!(
        !records(&journal(&s)).is_empty(),
        "journal should have records before abort"
    );

    s.cli(&["abort", "--force"]).expect("abort");

    let recs = records(&journal(&s));
    assert_eq!(
        recs.len(),
        1,
        "journal should contain only the phantom meta after abort: {recs:?}"
    );
}

/// Commit preserves the journal file inode (truncates, doesn't delete+recreate).
/// The kernel holds an O_APPEND fd to the journal; deleting would make it stale.
#[test]
fn commit_preserves_journal_inode() {
    let s = YoloSession::new().expect("session setup");
    let journal_path = s.root.join(".yolofs/journal");

    fs::write(s.mnt_path("hello.txt"), "modified\n").expect("write");
    let ino_before = fs::metadata(&journal_path).expect("stat before").ino();

    s.cli(&["commit"]).expect("commit");

    let meta = fs::metadata(&journal_path).expect("journal file should still exist");
    assert_eq!(
        meta.ino(),
        ino_before,
        "journal inode must be preserved across commit"
    );
    assert_eq!(meta.len(), 0, "journal should be truncated to zero length");
}

/// Abort preserves the journal file inode (truncates, doesn't delete+recreate).
/// The kernel holds an O_APPEND fd to the journal; deleting would make it stale.
#[test]
fn abort_preserves_journal_inode() {
    let s = YoloSession::new().expect("session setup");
    let journal_path = s.root.join(".yolofs/journal");

    fs::write(s.mnt_path("hello.txt"), "modified\n").expect("write");
    let ino_before = fs::metadata(&journal_path).expect("stat before").ino();

    s.cli(&["abort", "--force"]).expect("abort");

    let meta = fs::metadata(&journal_path).expect("journal file should still exist");
    assert_eq!(
        meta.ino(),
        ino_before,
        "journal inode must be preserved across abort"
    );
    assert_eq!(meta.len(), 0, "journal should be truncated to zero length");
}

// ── Inode Store ──────────────────────────────────────────────────────────────────

/// After commit, the inode store is empty.
#[test]
fn commit_empties_inode_store() {
    let s = YoloSession::new().expect("session setup");

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
    let s = YoloSession::new().expect("session setup");

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

/// Commit preserves the inodes directory inode (removes entries individually,
/// doesn't rm -rf + mkdir). This keeps the directory inode stable.
#[test]
fn commit_preserves_inodes_dir_inode() {
    let s = YoloSession::new().expect("session setup");
    let inodes_dir = s.root.join(".yolofs/inodes");

    fs::write(s.mnt_path("hello.txt"), "modified\n").expect("write");
    let ino_before = fs::metadata(&inodes_dir).expect("stat before").ino();

    s.cli(&["commit"]).expect("commit");

    let ino_after = fs::metadata(&inodes_dir)
        .expect("inodes dir should still exist")
        .ino();
    assert_eq!(
        ino_before, ino_after,
        "inodes directory inode must be preserved across commit"
    );
}

/// Abort preserves the inodes directory inode (removes entries individually,
/// doesn't rm -rf + mkdir). This keeps the directory inode stable.
#[test]
fn abort_preserves_inodes_dir_inode() {
    let s = YoloSession::new().expect("session setup");
    let inodes_dir = s.root.join(".yolofs/inodes");

    fs::write(s.mnt_path("hello.txt"), "modified\n").expect("write");
    let ino_before = fs::metadata(&inodes_dir).expect("stat before").ino();

    s.cli(&["abort", "--force"]).expect("abort");

    let ino_after = fs::metadata(&inodes_dir)
        .expect("inodes dir should still exist")
        .ino();
    assert_eq!(
        ino_before, ino_after,
        "inodes directory inode must be preserved across abort"
    );
}

/// Travel to a snapshot preserves all inodes (orphans cleaned up on commit/abort).
#[test]
fn travel_preserves_all_inodes() {
    let s = YoloSession::new().expect("session setup");

    fs::write(s.mnt_path("hello.txt"), "pre-chk\n").expect("write pre");
    s.cli(&["snapshot", "s1"]).expect("snapshot");
    fs::write(s.mnt_path("multi.txt"), "post-chk\n").expect("write post");

    // Grab the post-snapshot inode id before travel
    let ch = tree(&s);
    let post_id = ino_for(&ch, "/multi.txt");

    s.cli(&["travel", "s1"]).expect("travel");

    // Post-snapshot inode should still exist on disk (orphaned)
    let remaining = inos(&s);
    assert!(
        remaining.contains(&post_id),
        "post-snapshot inode should survive travel: remaining={remaining:?}"
    );
    assert_eq!(
        fs::read_to_string(inode_path(&s, post_id)).unwrap(),
        "post-chk\n",
        "post-snapshot inode content should be intact"
    );
}

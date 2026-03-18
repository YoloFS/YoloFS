use super::helpers::{changes, ino_for, inode_path, journal};
use crate::helpers::AgfsSession;
use agfs::journal::Record;
use std::fs;

// ── Journal ──────────────────────────────────────────────────────────────────

/// Creating a checkpoint produces a Checkpoint record with the given name.
#[test]
fn checkpoint_produces_checkpoint_record() {
    let s = AgfsSession::new().expect("session setup");

    fs::write(s.mnt_path("hello.txt"), "modified\n").expect("write");
    s.cli(&["checkpoint", "build"]).expect("checkpoint");

    let records = journal(&s);
    assert!(
        records
            .iter()
            .any(|r| matches!(r, Record::Checkpoint(c) if c.name == "build")),
        "journal should have an S record named 'build': {records:?}"
    );
}

/// Write after checkpoint produces a new Add (re-COW) with a different ino.
#[test]
fn recow_after_checkpoint_produces_new_add() {
    let s = AgfsSession::new().expect("session setup");

    fs::write(s.mnt_path("hello.txt"), "v1\n").expect("write v1");
    s.cli(&["checkpoint", "s1"]).expect("checkpoint");
    fs::write(s.mnt_path("hello.txt"), "v2\n").expect("write v2 (re-COW)");

    let records = journal(&s);
    let adds: Vec<_> = records
        .iter()
        .filter(|r| matches!(r, Record::Modified { path, .. } if path.ends_with("/hello.txt")))
        .collect();
    assert!(
        adds.len() >= 2,
        "re-COW should produce a second A record: {records:?}"
    );

    // The two adds should have different ino values (re-COW allocates a new inode)
    if let (Record::Modified { ino: ino1, .. }, Record::Modified { ino: ino2, .. }) =
        (adds[0], adds[1])
    {
        assert_ne!(ino1, ino2, "re-COW ino values should differ: {records:?}");
    }

    // Checkpoint "s1" should sit between the two adds.
    let chk_pos = records
        .iter()
        .position(|r| matches!(r, Record::Checkpoint(c) if c.name == "s1"))
        .unwrap();
    let first_add = records
        .iter()
        .position(|r| matches!(r, Record::Modified { path, .. } if path.ends_with("/hello.txt")))
        .unwrap();
    let last_add = records
        .iter()
        .rposition(|r| matches!(r, Record::Modified { path, .. } if path.ends_with("/hello.txt")))
        .unwrap();
    assert!(
        first_add < chk_pos,
        "first Add should precede Checkpoint s1"
    );
    assert!(
        chk_pos < last_add,
        "Checkpoint s1 should precede re-COW Add"
    );
}

/// Multiple checkpoints interleaved with writes: each checkpoint gets a unique id.
#[test]
fn multiple_checkpoints_have_distinct_ids() {
    let s = AgfsSession::new().expect("session setup");

    fs::write(s.mnt_path("hello.txt"), "v1\n").expect("write");
    s.cli(&["checkpoint", "s1"]).expect("checkpoint s1");
    fs::write(s.mnt_path("hello.txt"), "v2\n").expect("write");
    s.cli(&["checkpoint", "s2"]).expect("checkpoint s2");

    let records = journal(&s);
    let snaps: Vec<_> = records
        .iter()
        .filter_map(|r| match r {
            Record::Checkpoint(c) if c.name != "(initial)" => Some((&c.id, &c.name)),
            _ => None,
        })
        .collect();
    assert_eq!(
        snaps.len(),
        2,
        "should have 2 user checkpoint records: {records:?}"
    );
    assert_ne!(snaps[0].0, snaps[1].0, "checkpoint ids should differ");
    assert_eq!(snaps[0].1, "s1");
    assert_eq!(snaps[1].1, "s2");
}

/// Rename after checkpoint: the Delete + Staged records appear after the K record.
/// Writing to a base file triggers COW (staged inode), then renaming keeps the ino.
#[test]
fn rename_after_checkpoint() {
    let s = AgfsSession::new().expect("session setup");

    fs::write(s.mnt_path("hello.txt"), "modified\n").expect("write");
    s.cli(&["checkpoint", "s1"]).expect("checkpoint");
    fs::rename(s.mnt_path("hello.txt"), s.mnt_path("moved.txt")).expect("rename");

    let records = journal(&s);
    let chk_pos = records
        .iter()
        .position(|r| matches!(r, Record::Checkpoint(_)))
        .unwrap();
    // After COW, hello.txt has a staged ino — rename emits Delete + Staged
    let del_pos = records
        .iter()
        .position(|r| matches!(r, Record::Deleted { path } if path.ends_with("/hello.txt")))
        .expect("should have Delete for hello.txt");
    let staged_pos = records
        .iter()
        .position(|r| matches!(r, Record::Added { path, .. } if path.ends_with("/moved.txt")))
        .expect("should have Added for moved.txt");
    assert!(
        chk_pos < del_pos,
        "Checkpoint should precede Delete: {records:?}"
    );
    assert!(
        del_pos < staged_pos,
        "Delete should precede Staged at new name: {records:?}"
    );
}

/// Delete after checkpoint: the D record appears after the S record.
#[test]
fn delete_after_checkpoint() {
    let s = AgfsSession::new().expect("session setup");

    fs::write(s.mnt_path("hello.txt"), "modified\n").expect("write");
    s.cli(&["checkpoint", "s1"]).expect("checkpoint");
    fs::remove_file(s.mnt_path("hello.txt")).expect("delete");

    let records = journal(&s);
    let chk_pos = records
        .iter()
        .position(|r| matches!(r, Record::Checkpoint(_)))
        .unwrap();
    let del_pos = records
        .iter()
        .position(|r| matches!(r, Record::Deleted { .. }))
        .unwrap();
    assert!(
        chk_pos < del_pos,
        "Checkpoint should precede Delete: {records:?}"
    );
}

// ── Inode Store ──────────────────────────────────────────────────────────────────

/// After checkpoint + re-COW, the pre-checkpoint inode is preserved with old content.
#[test]
fn recow_preserves_pre_checkpoint_inode() {
    let s = AgfsSession::new().expect("session setup");

    fs::write(s.mnt_path("hello.txt"), "v1\n").expect("write v1");

    let ch_v1 = changes(&s);
    let id_v1 = ino_for(&ch_v1, "/hello.txt");

    s.cli(&["checkpoint", "s1"]).expect("checkpoint");
    fs::write(s.mnt_path("hello.txt"), "v2\n").expect("write v2 (re-COW)");

    // v1 inode should still have old content
    assert_eq!(
        fs::read_to_string(inode_path(&s, id_v1)).unwrap(),
        "v1\n",
        "pre-checkpoint inode should be preserved with v1 content"
    );

    // v2 should be in a different inode
    let ch_v2 = changes(&s);
    let id_v2 = ino_for(&ch_v2, "/hello.txt");
    assert_ne!(id_v1, id_v2, "re-COW should allocate a new inode ID");
    assert_eq!(
        fs::read_to_string(inode_path(&s, id_v2)).unwrap(),
        "v2\n",
        "current inode should have v2 content"
    );
}

/// Multiple checkpoints preserve each version's inode independently.
#[test]
fn multiple_checkpoints_preserve_all_inodes() {
    let s = AgfsSession::new().expect("session setup");

    // v1 → chk → v2 → chk → v3
    fs::write(s.mnt_path("hello.txt"), "v1\n").expect("write v1");
    let id_v1 = ino_for(&changes(&s), "/hello.txt");

    s.cli(&["checkpoint", "s1"]).expect("checkpoint s1");
    fs::write(s.mnt_path("hello.txt"), "v2\n").expect("write v2");
    let id_v2 = ino_for(&changes(&s), "/hello.txt");

    s.cli(&["checkpoint", "s2"]).expect("checkpoint s2");
    fs::write(s.mnt_path("hello.txt"), "v3\n").expect("write v3");
    let id_v3 = ino_for(&changes(&s), "/hello.txt");

    // All three inode IDs should be different
    assert_ne!(id_v1, id_v2);
    assert_ne!(id_v2, id_v3);
    assert_ne!(id_v1, id_v3);

    // Each inode should have the correct content
    assert_eq!(fs::read_to_string(inode_path(&s, id_v1)).unwrap(), "v1\n");
    assert_eq!(fs::read_to_string(inode_path(&s, id_v2)).unwrap(), "v2\n");
    assert_eq!(fs::read_to_string(inode_path(&s, id_v3)).unwrap(), "v3\n");
}

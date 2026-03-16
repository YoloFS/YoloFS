use super::helpers::{changes, ino_for, inode_path, journal};
use crate::helpers::AgfsSession;
use agfs::journal::Record;
use std::fs;

// ── Journal ──────────────────────────────────────────────────────────────────

/// Creating a snapshot produces a Snapshot record with the given name.
#[test]
fn snapshot_produces_snapshot_record() {
    let s = AgfsSession::new().expect("session setup");

    fs::write(s.mnt_path("hello.txt"), "modified\n").expect("write");
    s.cli(&["snapshot", "build"]).expect("snapshot");

    let records = journal(&s);
    assert!(
        records
            .iter()
            .any(|r| matches!(r, Record::Snapshot { name, .. } if name == "build")),
        "journal should have an S record named 'build': {records:?}"
    );
}

/// Write after snapshot produces a new Add (re-COW) with a different ino.
#[test]
fn recow_after_snapshot_produces_new_add() {
    let s = AgfsSession::new().expect("session setup");

    fs::write(s.mnt_path("hello.txt"), "v1\n").expect("write v1");
    s.cli(&["snapshot", "s1"]).expect("snapshot");
    fs::write(s.mnt_path("hello.txt"), "v2\n").expect("write v2 (re-COW)");

    let records = journal(&s);
    let adds: Vec<_> = records
        .iter()
        .filter(|r| matches!(r, Record::Add { path, .. } if path.ends_with("/hello.txt")))
        .collect();
    assert!(
        adds.len() >= 2,
        "re-COW should produce a second A record: {records:?}"
    );

    // The two adds should have different ino values (re-COW allocates a new inode)
    if let (Record::Add { ino: ino1, .. }, Record::Add { ino: ino2, .. }) = (adds[0], adds[1]) {
        assert_ne!(ino1, ino2, "re-COW ino values should differ: {records:?}");
    }

    // Snapshot "s1" should sit between the two adds.
    let snap_pos = records
        .iter()
        .position(|r| matches!(r, Record::Snapshot { name, .. } if name == "s1"))
        .unwrap();
    let first_add = records
        .iter()
        .position(|r| matches!(r, Record::Add { path, .. } if path.ends_with("/hello.txt")))
        .unwrap();
    let last_add = records
        .iter()
        .rposition(|r| matches!(r, Record::Add { path, .. } if path.ends_with("/hello.txt")))
        .unwrap();
    assert!(first_add < snap_pos, "first Add should precede Snapshot s1");
    assert!(snap_pos < last_add, "Snapshot s1 should precede re-COW Add");
}

/// Multiple snapshots interleaved with writes: each snapshot gets a unique id.
#[test]
fn multiple_snapshots_have_distinct_ids() {
    let s = AgfsSession::new().expect("session setup");

    fs::write(s.mnt_path("hello.txt"), "v1\n").expect("write");
    s.cli(&["snapshot", "s1"]).expect("snapshot s1");
    fs::write(s.mnt_path("hello.txt"), "v2\n").expect("write");
    s.cli(&["snapshot", "s2"]).expect("snapshot s2");

    let records = journal(&s);
    let snaps: Vec<_> = records
        .iter()
        .filter_map(|r| match r {
            Record::Snapshot { id, name } if name != "(initial)" => Some((id, name)),
            _ => None,
        })
        .collect();
    assert_eq!(
        snaps.len(),
        2,
        "should have 2 user snapshot records: {records:?}"
    );
    assert_ne!(snaps[0].0, snaps[1].0, "snapshot ids should differ");
    assert_eq!(snaps[0].1, "s1");
    assert_eq!(snaps[1].1, "s2");
}

/// Rename after snapshot: the R record appears after the S record.
#[test]
fn rename_after_snapshot() {
    let s = AgfsSession::new().expect("session setup");

    fs::write(s.mnt_path("hello.txt"), "modified\n").expect("write");
    s.cli(&["snapshot", "s1"]).expect("snapshot");
    fs::rename(s.mnt_path("hello.txt"), s.mnt_path("moved.txt")).expect("rename");

    let records = journal(&s);
    let snap_pos = records
        .iter()
        .position(|r| matches!(r, Record::Snapshot { .. }))
        .unwrap();
    let ren_pos = records
        .iter()
        .position(|r| matches!(r, Record::Rename { .. }))
        .unwrap();
    assert!(
        snap_pos < ren_pos,
        "Snapshot should precede Rename: {records:?}"
    );
}

/// Delete after snapshot: the D record appears after the S record.
#[test]
fn delete_after_snapshot() {
    let s = AgfsSession::new().expect("session setup");

    fs::write(s.mnt_path("hello.txt"), "modified\n").expect("write");
    s.cli(&["snapshot", "s1"]).expect("snapshot");
    fs::remove_file(s.mnt_path("hello.txt")).expect("delete");

    let records = journal(&s);
    let snap_pos = records
        .iter()
        .position(|r| matches!(r, Record::Snapshot { .. }))
        .unwrap();
    let del_pos = records
        .iter()
        .position(|r| matches!(r, Record::Delete { .. }))
        .unwrap();
    assert!(
        snap_pos < del_pos,
        "Snapshot should precede Delete: {records:?}"
    );
}

// ── Inode Store ──────────────────────────────────────────────────────────────────

/// After snapshot + re-COW, the pre-snapshot inode is preserved with old content.
#[test]
fn recow_preserves_pre_snapshot_inode() {
    let s = AgfsSession::new().expect("session setup");

    fs::write(s.mnt_path("hello.txt"), "v1\n").expect("write v1");

    let ch_v1 = changes(&s);
    let id_v1 = ino_for(&ch_v1, "/hello.txt");

    s.cli(&["snapshot", "s1"]).expect("snapshot");
    fs::write(s.mnt_path("hello.txt"), "v2\n").expect("write v2 (re-COW)");

    // v1 inode should still have old content
    assert_eq!(
        fs::read_to_string(inode_path(&s, id_v1)).unwrap(),
        "v1\n",
        "pre-snapshot inode should be preserved with v1 content"
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

/// Multiple snapshots preserve each version's inode independently.
#[test]
fn multiple_snapshots_preserve_all_inodes() {
    let s = AgfsSession::new().expect("session setup");

    // v1 → snap → v2 → snap → v3
    fs::write(s.mnt_path("hello.txt"), "v1\n").expect("write v1");
    let id_v1 = ino_for(&changes(&s), "/hello.txt");

    s.cli(&["snapshot", "s1"]).expect("snapshot s1");
    fs::write(s.mnt_path("hello.txt"), "v2\n").expect("write v2");
    let id_v2 = ino_for(&changes(&s), "/hello.txt");

    s.cli(&["snapshot", "s2"]).expect("snapshot s2");
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

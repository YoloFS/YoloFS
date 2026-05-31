use super::helpers::{ino_for, inode_path, journal, markers, records, tree};
use crate::helpers::YoloSession;
use std::fs;
use yolofs::journal::{Action, Marker, Record};

// ── Journal ──────────────────────────────────────────────────────────────────

/// Creating a snapshot produces a Snapshot record with the given name.
#[test]
fn snapshot_produces_snapshot_record() {
    let s = YoloSession::new().expect("session setup");

    fs::write(s.mnt_path("hello.txt"), "modified\n").expect("write");
    s.cli(&["snapshot", "build"]).expect("snapshot");

    let j = journal(&s);
    let mkrs = markers(&j);
    assert!(
        mkrs.iter()
            .any(|m| matches!(m, Marker::Snapshot { name, .. } if name == "build")),
        "journal should have a Snapshot record named 'build': {mkrs:?}"
    );
}

/// Write after snapshot produces a new Add (re-COW) with a different ino.
#[test]
fn recow_after_snapshot_produces_new_add() {
    let s = YoloSession::new().expect("session setup");

    fs::write(s.mnt_path("hello.txt"), "v1\n").expect("write v1");
    s.cli(&["snapshot", "s1"]).expect("snapshot");
    fs::write(s.mnt_path("hello.txt"), "v2\n").expect("write v2 (re-COW)");

    let recs = records(&journal(&s));
    let adds: Vec<_> = recs
        .iter()
        .filter(|r| matches!(r, Record::Action(Action::Stage { path, .. }) if path.ends_with("/hello.txt")))
        .collect();
    assert!(
        adds.len() >= 2,
        "re-COW should produce a second Stage record: {recs:?}"
    );

    // The two adds should have different ino values (re-COW allocates a new inode)
    if let (
        Record::Action(Action::Stage { ino: ino1, .. }),
        Record::Action(Action::Stage { ino: ino2, .. }),
    ) = (adds[0], adds[1])
    {
        assert_ne!(ino1, ino2, "re-COW ino values should differ: {recs:?}");
    }

    // Snapshot "s1" should sit between the two adds.
    let chk_pos = recs
        .iter()
        .position(|r| matches!(r, Record::Marker(Marker::Snapshot { name, .. }) if name == "s1"))
        .unwrap();
    let first_add = recs
        .iter()
        .position(|r| matches!(r, Record::Action(Action::Stage { path, .. }) if path.ends_with("/hello.txt")))
        .unwrap();
    let last_add = recs
        .iter()
        .rposition(|r| matches!(r, Record::Action(Action::Stage { path, .. }) if path.ends_with("/hello.txt")))
        .unwrap();
    assert!(first_add < chk_pos, "first Add should precede Snapshot s1");
    assert!(chk_pos < last_add, "Snapshot s1 should precede re-COW Add");
}

/// Multiple snapshots interleaved with writes: each snapshot gets a unique id.
#[test]
fn multiple_snapshots_have_distinct_ids() {
    let s = YoloSession::new().expect("session setup");

    fs::write(s.mnt_path("hello.txt"), "v1\n").expect("write");
    s.cli(&["snapshot", "s1"]).expect("snapshot s1");
    fs::write(s.mnt_path("hello.txt"), "v2\n").expect("write");
    s.cli(&["snapshot", "s2"]).expect("snapshot s2");

    let j = journal(&s);
    let mkrs = markers(&j);
    let snaps: Vec<_> = mkrs
        .iter()
        .filter_map(|m| match m {
            Marker::Snapshot { gen_id, name } => Some((gen_id, name)),
            _ => None,
        })
        .collect();
    assert_eq!(
        snaps.len(),
        3,
        "should have phantom + 2 user snapshot records: {mkrs:?}"
    );
    assert_eq!(snaps[0].1, "(initial)");
    assert_eq!(snaps[1].1, "s1");
    assert_eq!(snaps[2].1, "s2");
    assert_ne!(snaps[1].0, snaps[2].0, "user snapshot ids should differ");
}

/// Rename after snapshot: the R record appears after the Snapshot record.
/// Writing to a base file triggers COW (staged inode), then renaming emits R.
#[test]
fn rename_after_snapshot() {
    let s = YoloSession::new().expect("session setup");

    fs::write(s.mnt_path("hello.txt"), "modified\n").expect("write");
    s.cli(&["snapshot", "s1"]).expect("snapshot");
    fs::rename(s.mnt_path("hello.txt"), s.mnt_path("moved.txt")).expect("rename");

    let recs = records(&journal(&s));
    let chk_pos = recs
        .iter()
        .position(|r| matches!(r, Record::Marker(Marker::Snapshot { name, .. }) if name == "s1"))
        .unwrap();
    // After COW, hello.txt has a staged ino — rename emits single R record
    let rename_pos = recs
        .iter()
        .position(|r| matches!(r, Record::Action(Action::Rename { dst, .. }) if dst.ends_with("/moved.txt")))
        .expect("should have Redirect for moved.txt");
    assert!(
        chk_pos < rename_pos,
        "Snapshot should precede Rename: {recs:?}"
    );
}

/// Delete after snapshot: the DEL record appears after the Snapshot record.
#[test]
fn delete_after_snapshot() {
    let s = YoloSession::new().expect("session setup");

    fs::write(s.mnt_path("hello.txt"), "modified\n").expect("write");
    s.cli(&["snapshot", "s1"]).expect("snapshot");
    fs::remove_file(s.mnt_path("hello.txt")).expect("delete");

    let recs = records(&journal(&s));
    let chk_pos = recs
        .iter()
        .position(|r| matches!(r, Record::Marker(Marker::Snapshot { name, .. }) if name == "s1"))
        .unwrap();
    let del_pos = recs
        .iter()
        .position(|r| matches!(r, Record::Action(Action::Delete { .. })))
        .unwrap();
    assert!(
        chk_pos < del_pos,
        "Snapshot should precede Delete: {recs:?}"
    );
}

// ── Inode Store ──────────────────────────────────────────────────────────────────

/// After snapshot + re-COW, the pre-snapshot inode is preserved with old content.
#[test]
fn recow_preserves_pre_snapshot_inode() {
    let s = YoloSession::new().expect("session setup");

    fs::write(s.mnt_path("hello.txt"), "v1\n").expect("write v1");

    let ch_v1 = tree(&s);
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
    let ch_v2 = tree(&s);
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
    let s = YoloSession::new().expect("session setup");

    // v1 → chk → v2 → chk → v3
    fs::write(s.mnt_path("hello.txt"), "v1\n").expect("write v1");
    let id_v1 = ino_for(&tree(&s), "/hello.txt");

    s.cli(&["snapshot", "s1"]).expect("snapshot s1");
    fs::write(s.mnt_path("hello.txt"), "v2\n").expect("write v2");
    let id_v2 = ino_for(&tree(&s), "/hello.txt");

    s.cli(&["snapshot", "s2"]).expect("snapshot s2");
    fs::write(s.mnt_path("hello.txt"), "v3\n").expect("write v3");
    let id_v3 = ino_for(&tree(&s), "/hello.txt");

    // All three inode IDs should be different
    assert_ne!(id_v1, id_v2);
    assert_ne!(id_v2, id_v3);
    assert_ne!(id_v1, id_v3);

    // Each inode should have the correct content
    assert_eq!(fs::read_to_string(inode_path(&s, id_v1)).unwrap(), "v1\n");
    assert_eq!(fs::read_to_string(inode_path(&s, id_v2)).unwrap(), "v2\n");
    assert_eq!(fs::read_to_string(inode_path(&s, id_v3)).unwrap(), "v3\n");
}

/// Writing an untouched base file after snapshot triggers COW correctly.
/// This exercises the path where the dentry's cached dirent pointer is NULL
/// (no prior staged entry), so yolo_read_dirent returns packed=0 (tombstone)
/// and the slow COW path runs.
#[test]
fn untouched_base_file_cow_after_snapshot() {
    let s = YoloSession::new().expect("session setup");

    // Touch hello.txt to make the session dirty, then snapshot.
    fs::write(s.mnt_path("hello.txt"), "dirty\n").expect("write");
    s.cli(&["snapshot", "s1"]).expect("snapshot");

    // multi.txt was never touched — its dirent pointer is NULL.
    fs::write(s.mnt_path("multi.txt"), "updated\n").expect("write untouched base file");

    // Verify the written content is readable through the mount.
    assert_eq!(
        fs::read_to_string(s.mnt_path("multi.txt")).unwrap(),
        "updated\n",
        "untouched base file should be readable after COW"
    );

    // Verify a new inode was allocated in the store.
    let ch = tree(&s);
    let ino = ino_for(&ch, "/multi.txt");
    assert_eq!(
        fs::read_to_string(inode_path(&s, ino)).unwrap(),
        "updated\n",
        "COW inode should contain written content"
    );
}

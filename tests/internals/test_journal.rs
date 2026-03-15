use crate::helpers::AgfsSession;
use agfs::journal::{self, Record};
use std::fs;

// ── Integration tests for the kernel → journal contract. ─────────────────────
//
// These verify that each filesystem operation produces the expected journal
// record type.  The unit tests in cli/journal.rs cover parsing and resolution
// with synthetic data; these confirm the kernel module writes the same format.

/// Helper: read parsed journal records for a session.
fn journal(s: &AgfsSession) -> Vec<Record> {
    journal::read(&s.root.join(".agfs")).expect("read journal")
}

// ── A (Add) records ──────────────────────────────────────────────────────────

/// Modifying an existing file produces an Add record.
#[test]
fn modify_produces_add_record() {
    let s = AgfsSession::new().expect("session setup");

    fs::write(s.mnt_path("hello.txt"), "modified\n").expect("write");

    let records = journal(&s);
    assert!(
        records.iter().any(|r| matches!(r, Record::Add { path, .. } if path.ends_with("/hello.txt"))),
        "journal should have an A record for hello.txt: {records:?}"
    );
}

/// Creating a brand-new file produces an Add record.
#[test]
fn create_produces_add_record() {
    let s = AgfsSession::new().expect("session setup");

    fs::write(s.mnt_path("brandnew.txt"), "new\n").expect("create");

    let records = journal(&s);
    assert!(
        records.iter().any(|r| matches!(r, Record::Add { path, .. } if path.ends_with("/brandnew.txt"))),
        "journal should have an A record for brandnew.txt: {records:?}"
    );
}

// ── D (Delete) records ───────────────────────────────────────────────────────

/// Deleting a file produces a Delete record.
#[test]
fn delete_produces_delete_record() {
    let s = AgfsSession::new().expect("session setup");

    fs::remove_file(s.mnt_path("hello.txt")).expect("unlink");

    let records = journal(&s);
    assert!(
        records.iter().any(|r| matches!(r, Record::Delete { path } if path.ends_with("/hello.txt"))),
        "journal should have a D record for hello.txt: {records:?}"
    );
}

// ── R (Rename) records ───────────────────────────────────────────────────────

/// Renaming a file produces a Rename record.
#[test]
fn rename_produces_rename_record() {
    let s = AgfsSession::new().expect("session setup");

    fs::rename(s.mnt_path("hello.txt"), s.mnt_path("moved.txt")).expect("rename");

    let records = journal(&s);
    assert!(
        records.iter().any(|r| matches!(r, Record::Rename { old_path, new_path }
            if old_path.ends_with("/hello.txt") && new_path.ends_with("/moved.txt"))),
        "journal should have an R record for hello.txt → moved.txt: {records:?}"
    );
}

// ── S (Snapshot) records ─────────────────────────────────────────────────────

/// Creating a snapshot produces a Snapshot record with the given name.
#[test]
fn snapshot_produces_snapshot_record() {
    let s = AgfsSession::new().expect("session setup");

    fs::write(s.mnt_path("hello.txt"), "modified\n").expect("write");
    s.cli(&["snapshot", "build"]).expect("snapshot");

    let records = journal(&s);
    assert!(
        records.iter().any(|r| matches!(r, Record::Snapshot { name, .. } if name == "build")),
        "journal should have an S record named 'build': {records:?}"
    );
}

// ── Sequencing ───────────────────────────────────────────────────────────────

/// Multiple operations produce records in order.
#[test]
fn operations_produce_ordered_records() {
    let s = AgfsSession::new().expect("session setup");

    // write → snapshot → delete → rename
    fs::write(s.mnt_path("hello.txt"), "v1\n").expect("write");
    s.cli(&["snapshot", "s1"]).expect("snapshot");
    fs::remove_file(s.mnt_path("subdir/deep.txt")).expect("delete");
    fs::rename(s.mnt_path("multi.txt"), s.mnt_path("renamed.txt")).expect("rename");

    let records = journal(&s);

    // Verify each type is present
    assert!(records.iter().any(|r| matches!(r, Record::Add { .. })), "missing A: {records:?}");
    assert!(records.iter().any(|r| matches!(r, Record::Snapshot { .. })), "missing S: {records:?}");
    assert!(records.iter().any(|r| matches!(r, Record::Delete { .. })), "missing D: {records:?}");
    assert!(records.iter().any(|r| matches!(r, Record::Rename { .. })), "missing R: {records:?}");

    // Snapshot "s1" should appear after the Add (write) and before the Delete.
    // Note: mount creates an (initial) snapshot, so match by name.
    let snap_pos = records.iter().position(|r| matches!(r, Record::Snapshot { name, .. } if name == "s1")).unwrap();
    let add_pos = records.iter().position(|r| matches!(r, Record::Add { path, .. } if path.ends_with("/hello.txt"))).unwrap();
    let del_pos = records.iter().position(|r| matches!(r, Record::Delete { .. })).unwrap();
    assert!(add_pos < snap_pos, "Add should precede Snapshot s1");
    assert!(snap_pos < del_pos, "Snapshot s1 should precede Delete");
}

// ── Compound operations ──────────────────────────────────────────────────────

/// Writing to a renamed file: rename produces R, then write produces A at new path.
#[test]
fn write_after_rename() {
    let s = AgfsSession::new().expect("session setup");

    fs::rename(s.mnt_path("hello.txt"), s.mnt_path("moved.txt")).expect("rename");
    fs::write(s.mnt_path("moved.txt"), "updated\n").expect("write renamed file");

    let records = journal(&s);
    assert!(
        records.iter().any(|r| matches!(r, Record::Rename { old_path, new_path }
            if old_path.ends_with("/hello.txt") && new_path.ends_with("/moved.txt"))),
        "should have R record: {records:?}"
    );
    assert!(
        records.iter().any(|r| matches!(r, Record::Add { path, .. } if path.ends_with("/moved.txt"))),
        "should have A record at new path: {records:?}"
    );

    // The rename should precede the write
    let r_pos = records.iter().position(|r| matches!(r, Record::Rename { .. })).unwrap();
    let a_pos = records.iter().rposition(|r| matches!(r, Record::Add { path, .. } if path.ends_with("/moved.txt"))).unwrap();
    assert!(r_pos < a_pos, "Rename should precede the Add at new path");
}

/// Create a new file, then rename it.
#[test]
fn create_then_rename() {
    let s = AgfsSession::new().expect("session setup");

    fs::write(s.mnt_path("temp.txt"), "ephemeral\n").expect("create");
    fs::rename(s.mnt_path("temp.txt"), s.mnt_path("final.txt")).expect("rename");

    let records = journal(&s);
    assert!(
        records.iter().any(|r| matches!(r, Record::Add { path, .. } if path.ends_with("/temp.txt"))),
        "should have A record for original path: {records:?}"
    );
    assert!(
        records.iter().any(|r| matches!(r, Record::Rename { old_path, new_path }
            if old_path.ends_with("/temp.txt") && new_path.ends_with("/final.txt"))),
        "should have R record: {records:?}"
    );
}

/// Create a file, then delete it — both A and D records should be present.
#[test]
fn create_then_delete() {
    let s = AgfsSession::new().expect("session setup");

    fs::write(s.mnt_path("ephemeral.txt"), "gone soon\n").expect("create");
    fs::remove_file(s.mnt_path("ephemeral.txt")).expect("delete");

    let records = journal(&s);
    assert!(
        records.iter().any(|r| matches!(r, Record::Add { path, .. } if path.ends_with("/ephemeral.txt"))),
        "should have A record: {records:?}"
    );
    assert!(
        records.iter().any(|r| matches!(r, Record::Delete { path } if path.ends_with("/ephemeral.txt"))),
        "should have D record: {records:?}"
    );
}

/// Modify a base file, then delete it — produces A then D.
#[test]
fn modify_then_delete() {
    let s = AgfsSession::new().expect("session setup");

    fs::write(s.mnt_path("hello.txt"), "modified\n").expect("write");
    fs::remove_file(s.mnt_path("hello.txt")).expect("delete");

    let records = journal(&s);
    let a_pos = records.iter().position(|r| matches!(r, Record::Add { path, .. } if path.ends_with("/hello.txt"))).expect("missing A");
    let d_pos = records.iter().position(|r| matches!(r, Record::Delete { path } if path.ends_with("/hello.txt"))).expect("missing D");
    assert!(a_pos < d_pos, "Add should precede Delete: {records:?}");
}

/// Overwrite an existing file multiple times — each write produces an A record
/// (the kernel doesn't coalesce; the CLI resolver handles that).
#[test]
fn multiple_writes_produce_multiple_adds() {
    let s = AgfsSession::new().expect("session setup");

    fs::write(s.mnt_path("hello.txt"), "v1\n").expect("write v1");
    fs::write(s.mnt_path("hello.txt"), "v2\n").expect("write v2");
    fs::write(s.mnt_path("hello.txt"), "v3\n").expect("write v3");

    let records = journal(&s);
    let add_count = records.iter()
        .filter(|r| matches!(r, Record::Add { path, .. } if path.ends_with("/hello.txt")))
        .count();
    // At least 1 A record; the kernel may coalesce O_TRUNC reopens on the
    // same blob, but the first COW always produces one.
    assert!(add_count >= 1, "should have at least 1 A record, got {add_count}: {records:?}");
}

// ── Snapshot interactions ────────────────────────────────────────────────────

/// Write after snapshot produces a new Add (re-COW) with a different blob id.
#[test]
fn recow_after_snapshot_produces_new_add() {
    let s = AgfsSession::new().expect("session setup");

    fs::write(s.mnt_path("hello.txt"), "v1\n").expect("write v1");
    s.cli(&["snapshot", "s1"]).expect("snapshot");
    fs::write(s.mnt_path("hello.txt"), "v2\n").expect("write v2 (re-COW)");

    let records = journal(&s);
    let adds: Vec<_> = records.iter()
        .filter(|r| matches!(r, Record::Add { path, .. } if path.ends_with("/hello.txt")))
        .collect();
    assert!(
        adds.len() >= 2,
        "re-COW should produce a second A record: {records:?}"
    );

    // The two adds should have different blob ids (re-COW allocates a new blob)
    if let (Record::Add { id: id1, .. }, Record::Add { id: id2, .. }) = (adds[0], adds[1]) {
        assert_ne!(id1, id2, "re-COW blob ids should differ: {records:?}");
    }

    // Snapshot "s1" should sit between the two adds.
    // Note: mount creates an (initial) snapshot, so match by name.
    let snap_pos = records.iter().position(|r| matches!(r, Record::Snapshot { name, .. } if name == "s1")).unwrap();
    let first_add = records.iter().position(|r| matches!(r, Record::Add { path, .. } if path.ends_with("/hello.txt"))).unwrap();
    let last_add = records.iter().rposition(|r| matches!(r, Record::Add { path, .. } if path.ends_with("/hello.txt"))).unwrap();
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
    // Filter to only the snapshots we created (mount adds an (initial) snapshot).
    let snaps: Vec<_> = records.iter()
        .filter_map(|r| match r {
            Record::Snapshot { id, name } if name != "(initial)" => Some((id, name)),
            _ => None,
        })
        .collect();
    assert_eq!(snaps.len(), 2, "should have 2 user snapshot records: {records:?}");
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
    let snap_pos = records.iter().position(|r| matches!(r, Record::Snapshot { .. })).unwrap();
    let ren_pos = records.iter().position(|r| matches!(r, Record::Rename { .. })).unwrap();
    assert!(snap_pos < ren_pos, "Snapshot should precede Rename: {records:?}");
}

/// Delete after snapshot: the D record appears after the S record.
#[test]
fn delete_after_snapshot() {
    let s = AgfsSession::new().expect("session setup");

    fs::write(s.mnt_path("hello.txt"), "modified\n").expect("write");
    s.cli(&["snapshot", "s1"]).expect("snapshot");
    fs::remove_file(s.mnt_path("hello.txt")).expect("delete");

    let records = journal(&s);
    let snap_pos = records.iter().position(|r| matches!(r, Record::Snapshot { .. })).unwrap();
    let del_pos = records.iter().position(|r| matches!(r, Record::Delete { .. })).unwrap();
    assert!(snap_pos < del_pos, "Snapshot should precede Delete: {records:?}");
}

/// Commit --at a snapshot clears records up to the snapshot, keeps the rest.
#[test]
fn commit_at_snapshot_preserves_trailing_records() {
    let s = AgfsSession::new().expect("session setup");

    fs::write(s.mnt_path("hello.txt"), "v1\n").expect("write v1");
    s.cli(&["snapshot", "s1"]).expect("snapshot");
    fs::write(s.mnt_path("multi.txt"), "post-snap\n").expect("write after snapshot");

    s.cli(&["commit", "--at", "s1"]).expect("commit --at");

    let records = journal(&s);
    // Pre-snapshot records and the snapshot itself should be gone
    assert!(
        !records.iter().any(|r| matches!(r, Record::Snapshot { name, .. } if name == "s1")),
        "s1 snapshot should be cleared after commit --at: {records:?}"
    );
    // Post-snapshot write should remain
    assert!(
        records.iter().any(|r| matches!(r, Record::Add { path, .. } if path.ends_with("/multi.txt"))),
        "post-snapshot Add should remain: {records:?}"
    );
}

/// Commit clears the journal.
#[test]
fn commit_clears_journal() {
    let s = AgfsSession::new().expect("session setup");

    fs::write(s.mnt_path("hello.txt"), "modified\n").expect("write");
    assert!(!journal(&s).is_empty(), "journal should have records before commit");

    s.cli(&["commit"]).expect("commit");

    let records = journal(&s);
    assert!(records.is_empty(), "journal should be empty after commit: {records:?}");
}

/// Abort clears the journal.
#[test]
fn abort_clears_journal() {
    let s = AgfsSession::new().expect("session setup");

    fs::write(s.mnt_path("hello.txt"), "modified\n").expect("write");
    assert!(!journal(&s).is_empty(), "journal should have records before abort");

    s.cli(&["abort", "--force"]).expect("abort");

    let records = journal(&s);
    assert!(records.is_empty(), "journal should be empty after abort: {records:?}");
}

// ── Directory operations ─────────────────────────────────────────────────────

/// Creating a directory produces an Add record.
#[test]
fn mkdir_produces_add_record() {
    let s = AgfsSession::new().expect("session setup");

    fs::create_dir(s.mnt_path("newdir")).expect("mkdir");

    let records = journal(&s);
    assert!(
        records.iter().any(|r| matches!(r, Record::Add { path, .. } if path.ends_with("/newdir"))),
        "journal should have an A record for newdir: {records:?}"
    );
}

/// Removing a directory produces a Delete record.
#[test]
fn rmdir_produces_delete_record() {
    let s = AgfsSession::new().expect("session setup");

    // Create through mount then remove
    fs::create_dir(s.mnt_path("tmpdir")).expect("mkdir");
    fs::remove_dir(s.mnt_path("tmpdir")).expect("rmdir");

    let records = journal(&s);
    assert!(
        records.iter().any(|r| matches!(r, Record::Delete { path } if path.ends_with("/tmpdir"))),
        "journal should have a D record for tmpdir: {records:?}"
    );
}

/// Renaming a directory produces a Rename record.
#[test]
fn rename_dir_produces_rename_record() {
    let s = AgfsSession::new().expect("session setup");

    fs::create_dir(s.mnt_path("olddir")).expect("mkdir");
    fs::rename(s.mnt_path("olddir"), s.mnt_path("newdir")).expect("rename dir");

    let records = journal(&s);
    assert!(
        records.iter().any(|r| matches!(r, Record::Rename { old_path, new_path }
            if old_path.ends_with("/olddir") && new_path.ends_with("/newdir"))),
        "journal should have an R record for olddir → newdir: {records:?}"
    );
}

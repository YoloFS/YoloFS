use super::helpers::journal;
use crate::helpers::AgfsSession;
use agfs::journal::Record;
use std::fs;

// ── Compound journal operations ──────────────────────────────────────────────
//
// These verify that compound filesystem operations produce the expected
// journal record sequence.  The CLI resolver collapses these into final
// changes; here we verify the raw kernel output.

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
    assert!(
        records.iter().any(|r| matches!(r, Record::Add { .. })),
        "missing A: {records:?}"
    );
    assert!(
        records.iter().any(|r| matches!(r, Record::Snapshot { .. })),
        "missing S: {records:?}"
    );
    assert!(
        records.iter().any(|r| matches!(r, Record::Delete { .. })),
        "missing D: {records:?}"
    );
    assert!(
        records.iter().any(|r| matches!(r, Record::Rename { .. })),
        "missing R: {records:?}"
    );

    // Snapshot "s1" should appear after the Add (write) and before the Delete.
    let snap_pos = records
        .iter()
        .position(|r| matches!(r, Record::Snapshot { name, .. } if name == "s1"))
        .unwrap();
    let add_pos = records
        .iter()
        .position(|r| matches!(r, Record::Add { path, .. } if path.ends_with("/hello.txt")))
        .unwrap();
    let del_pos = records
        .iter()
        .position(|r| matches!(r, Record::Delete { .. }))
        .unwrap();
    assert!(add_pos < snap_pos, "Add should precede Snapshot s1");
    assert!(snap_pos < del_pos, "Snapshot s1 should precede Delete");
}

/// Writing to a renamed file: rename produces R, then write produces A at new path.
#[test]
fn write_after_rename() {
    let s = AgfsSession::new().expect("session setup");

    fs::rename(s.mnt_path("hello.txt"), s.mnt_path("moved.txt")).expect("rename");
    fs::write(s.mnt_path("moved.txt"), "updated\n").expect("write renamed file");

    let records = journal(&s);
    assert!(
        records
            .iter()
            .any(|r| matches!(r, Record::Rename { old_path, new_path }
            if old_path.ends_with("/hello.txt") && new_path.ends_with("/moved.txt"))),
        "should have R record: {records:?}"
    );
    assert!(
        records
            .iter()
            .any(|r| matches!(r, Record::Add { path, .. } if path.ends_with("/moved.txt"))),
        "should have A record at new path: {records:?}"
    );

    // The rename should precede the write
    let r_pos = records
        .iter()
        .position(|r| matches!(r, Record::Rename { .. }))
        .unwrap();
    let a_pos = records
        .iter()
        .rposition(|r| matches!(r, Record::Add { path, .. } if path.ends_with("/moved.txt")))
        .unwrap();
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
        records
            .iter()
            .any(|r| matches!(r, Record::Add { path, .. } if path.ends_with("/temp.txt"))),
        "should have A record for original path: {records:?}"
    );
    assert!(
        records
            .iter()
            .any(|r| matches!(r, Record::Rename { old_path, new_path }
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
        records
            .iter()
            .any(|r| matches!(r, Record::Add { path, .. } if path.ends_with("/ephemeral.txt"))),
        "should have A record: {records:?}"
    );
    assert!(
        records
            .iter()
            .any(|r| matches!(r, Record::Delete { path } if path.ends_with("/ephemeral.txt"))),
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
    let a_pos = records
        .iter()
        .position(|r| matches!(r, Record::Add { path, .. } if path.ends_with("/hello.txt")))
        .expect("missing A");
    let d_pos = records
        .iter()
        .position(|r| matches!(r, Record::Delete { path } if path.ends_with("/hello.txt")))
        .expect("missing D");
    assert!(a_pos < d_pos, "Add should precede Delete: {records:?}");
}

/// Rename a file, then delete the new name — produces R then D.
#[test]
fn rename_then_delete() {
    let s = AgfsSession::new().expect("session setup");

    fs::rename(s.mnt_path("hello.txt"), s.mnt_path("moved.txt")).expect("rename");
    fs::remove_file(s.mnt_path("moved.txt")).expect("delete renamed file");

    let records = journal(&s);
    assert!(
        records
            .iter()
            .any(|r| matches!(r, Record::Rename { old_path, new_path }
            if old_path.ends_with("/hello.txt") && new_path.ends_with("/moved.txt"))),
        "should have R record: {records:?}"
    );
    assert!(
        records
            .iter()
            .any(|r| matches!(r, Record::Delete { path } if path.ends_with("/moved.txt"))),
        "should have D record at new path: {records:?}"
    );

    let r_pos = records
        .iter()
        .position(|r| matches!(r, Record::Rename { .. }))
        .expect("missing R");
    let d_pos = records
        .iter()
        .position(|r| matches!(r, Record::Delete { path } if path.ends_with("/moved.txt")))
        .expect("missing D");
    assert!(r_pos < d_pos, "Rename should precede Delete: {records:?}");
}

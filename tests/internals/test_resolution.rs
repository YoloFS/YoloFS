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

    // write → checkpoint → delete → rename
    fs::write(s.mnt_path("hello.txt"), "v1\n").expect("write");
    s.cli(&["checkpoint", "s1"]).expect("checkpoint");
    fs::remove_file(s.mnt_path("subdir/deep.txt")).expect("delete");
    fs::rename(s.mnt_path("multi.txt"), s.mnt_path("renamed.txt")).expect("rename");

    let records = journal(&s);

    // Verify each type is present
    assert!(
        records.iter().any(|r| matches!(r, Record::Modified { .. })),
        "missing A: {records:?}"
    );
    assert!(
        records
            .iter()
            .any(|r| matches!(r, Record::Checkpoint(_))),
        "missing S: {records:?}"
    );
    assert!(
        records.iter().any(|r| matches!(r, Record::Deleted { .. })),
        "missing D: {records:?}"
    );
    assert!(
        records.iter().any(|r| matches!(r, Record::Redirect { .. })),
        "missing R: {records:?}"
    );

    // Checkpoint "s1" should appear after the Add (write) and before the Delete.
    let chk_pos = records
        .iter()
        .position(|r| matches!(r, Record::Checkpoint(c) if c.name == "s1"))
        .unwrap();
    let add_pos = records
        .iter()
        .position(|r| matches!(r, Record::Modified { path, .. } if path.ends_with("/hello.txt")))
        .unwrap();
    let del_pos = records
        .iter()
        .position(|r| matches!(r, Record::Deleted { .. }))
        .unwrap();
    assert!(add_pos < chk_pos, "Add should precede Checkpoint s1");
    assert!(chk_pos < del_pos, "Checkpoint s1 should precede Delete");
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
            .any(|r| matches!(r, Record::Redirect { path, base, .. }
            if path.ends_with("/moved.txt") && base.ends_with("/hello.txt"))),
        "should have R record: {records:?}"
    );
    assert!(
        records
            .iter()
            .any(|r| matches!(r, Record::Modified { path, .. } if path.ends_with("/moved.txt"))),
        "should have A record at new path: {records:?}"
    );

    // The rename should precede the write
    let r_pos = records
        .iter()
        .position(|r| matches!(r, Record::Redirect { .. }))
        .unwrap();
    let a_pos = records
        .iter()
        .rposition(|r| matches!(r, Record::Modified { path, .. } if path.ends_with("/moved.txt")))
        .unwrap();
    assert!(r_pos < a_pos, "Rename should precede the Add at new path");
}

/// Create a new file, then rename it.
/// Staged file rename produces Delete + Staged (same ino), not Redirect.
#[test]
fn create_then_rename() {
    let s = AgfsSession::new().expect("session setup");

    fs::write(s.mnt_path("temp.txt"), "ephemeral\n").expect("create");
    fs::rename(s.mnt_path("temp.txt"), s.mnt_path("final.txt")).expect("rename");

    let records = journal(&s);
    assert!(
        records
            .iter()
            .any(|r| matches!(r, Record::Added { path, .. } if path.ends_with("/temp.txt"))),
        "should have A record for original path: {records:?}"
    );
    assert!(
        records
            .iter()
            .any(|r| matches!(r, Record::Added { path, .. }
            if path.ends_with("/final.txt"))),
        "should have Staged record at new path: {records:?}"
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
            .any(|r| matches!(r, Record::Added { path, .. } if path.ends_with("/ephemeral.txt"))),
        "should have A record: {records:?}"
    );
    assert!(
        records
            .iter()
            .any(|r| matches!(r, Record::Deleted { path } if path.ends_with("/ephemeral.txt"))),
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
        .position(|r| matches!(r, Record::Modified { path, .. } if path.ends_with("/hello.txt")))
        .expect("missing M");
    let d_pos = records
        .iter()
        .position(|r| matches!(r, Record::Deleted { path } if path.ends_with("/hello.txt")))
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
            .any(|r| matches!(r, Record::Redirect { path, base, .. }
            if path.ends_with("/moved.txt") && base.ends_with("/hello.txt"))),
        "should have R record: {records:?}"
    );
    assert!(
        records
            .iter()
            .any(|r| matches!(r, Record::Deleted { path } if path.ends_with("/moved.txt"))),
        "should have D record at new path: {records:?}"
    );

    let r_pos = records
        .iter()
        .position(|r| matches!(r, Record::Redirect { .. }))
        .expect("missing R");
    let d_pos = records
        .iter()
        .position(|r| matches!(r, Record::Deleted { path } if path.ends_with("/moved.txt")))
        .expect("missing D");
    assert!(r_pos < d_pos, "Rename should precede Delete: {records:?}");
}

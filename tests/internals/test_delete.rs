use crate::helpers::AgfsSession;
use agfs::journal::Record;
use super::helpers::{journal, changes, blob_entries, blob_path, blob_id_for};
use std::fs;

// ── Journal ──────────────────────────────────────────────────────────────────

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

// ── Staging ──────────────────────────────────────────────────────────────────

/// Deleting a file does NOT create a new blob (only a journal D record).
#[test]
fn delete_creates_no_blob() {
    let s = AgfsSession::new().expect("session setup");

    let blobs_before = blob_entries(&s);
    fs::remove_file(s.mnt_path("hello.txt")).expect("unlink");
    let blobs_after = blob_entries(&s);

    assert_eq!(
        blobs_before, blobs_after,
        "delete should not create new staging blobs"
    );
}

/// Delete then recreate a file: the new file gets a fresh blob.
#[test]
fn delete_recreate_gets_new_blob() {
    let s = AgfsSession::new().expect("session setup");

    fs::remove_file(s.mnt_path("hello.txt")).expect("delete");
    fs::write(s.mnt_path("hello.txt"), "reborn\n").expect("recreate");

    let ch = changes(&s);
    let id = blob_id_for(&ch, "/hello.txt");
    assert_eq!(
        fs::read_to_string(blob_path(&s, id)).unwrap(),
        "reborn\n",
        "recreated file blob should have the new content"
    );
}

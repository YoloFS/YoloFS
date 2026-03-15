use crate::helpers::AgfsSession;
use agfs::journal::Record;
use super::helpers::{journal, changes, blob_path, blob_id_for};
use std::fs;

// ── Journal ──────────────────────────────────────────────────────────────────

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

// ── Staging ──────────────────────────────────────────────────────────────────

/// Creating a new file produces a staging blob.
#[test]
fn create_file_produces_blob() {
    let s = AgfsSession::new().expect("session setup");

    fs::write(s.mnt_path("brandnew.txt"), "fresh content\n").expect("create");

    let ch = changes(&s);
    let id = blob_id_for(&ch, "/brandnew.txt");
    let blob = blob_path(&s, id);

    assert!(blob.is_file(), "new file blob should be a regular file");
    assert_eq!(fs::read_to_string(&blob).unwrap(), "fresh content\n");
}

/// An empty file (touch) creates an empty blob.
#[test]
fn empty_file_creates_empty_blob() {
    let s = AgfsSession::new().expect("session setup");

    fs::write(s.mnt_path("empty.txt"), "").expect("touch");

    let ch = changes(&s);
    let id = blob_id_for(&ch, "/empty.txt");
    let blob = blob_path(&s, id);

    assert!(blob.is_file(), "empty file blob should exist");
    assert_eq!(fs::read(&blob).unwrap().len(), 0, "blob should be 0 bytes");
}

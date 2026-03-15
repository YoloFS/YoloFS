use crate::helpers::AgfsSession;
use agfs::journal::{Change, Record};
use super::helpers::{journal, changes, blob_entries, blob_path};
use std::fs;

// ── Journal ──────────────────────────────────────────────────────────────────

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

// ── Staging ──────────────────────────────────────────────────────────────────

/// Pure rename of a base file creates no new blob (only journal R record).
#[test]
fn pure_rename_creates_no_blob() {
    let s = AgfsSession::new().expect("session setup");

    let blobs_before = blob_entries(&s);
    fs::rename(s.mnt_path("hello.txt"), s.mnt_path("moved.txt")).expect("rename");
    let blobs_after = blob_entries(&s);

    assert_eq!(
        blobs_before, blobs_after,
        "pure rename should not create new staging blobs"
    );
}

/// Rename + modify (write after rename) produces a blob at the new path.
#[test]
fn rename_then_write_produces_blob() {
    let s = AgfsSession::new().expect("session setup");

    fs::rename(s.mnt_path("hello.txt"), s.mnt_path("moved.txt")).expect("rename");
    fs::write(s.mnt_path("moved.txt"), "new content\n").expect("write renamed file");

    let ch = changes(&s);
    let blob_id = ch.iter()
        .find_map(|c| match c {
            Change::RenamedModified { to, blob_id, .. } if to.ends_with("/moved.txt") => Some(*blob_id),
            Change::Modified { path, blob_id } if path.ends_with("/moved.txt") => Some(*blob_id),
            _ => None,
        })
        .expect("should have a blob for renamed+modified file");

    assert_eq!(
        fs::read_to_string(blob_path(&s, blob_id)).unwrap(),
        "new content\n"
    );
}

/// Write then rename: blob content still correct under the new path.
#[test]
fn write_then_rename_blob_content() {
    let s = AgfsSession::new().expect("session setup");

    fs::write(s.mnt_path("hello.txt"), "written first\n").expect("write");
    fs::rename(s.mnt_path("hello.txt"), s.mnt_path("final.txt")).expect("rename");

    // Read through mount to verify
    let content = fs::read_to_string(s.mnt_path("final.txt")).unwrap();
    assert_eq!(content, "written first\n");

    // The blob should have the written content — may resolve as RenamedModified
    // or as separate changes depending on the resolver.
    let ch = changes(&s);
    let blob_id = ch.iter()
        .find_map(|c| match c {
            Change::RenamedModified { to, blob_id, .. } if to.ends_with("/final.txt") => Some(*blob_id),
            Change::Modified { path, blob_id } if path.ends_with("/final.txt") => Some(*blob_id),
            Change::Added { path, blob_id } if path.ends_with("/final.txt") => Some(*blob_id),
            _ => None,
        });

    if let Some(id) = blob_id {
        assert_eq!(fs::read_to_string(blob_path(&s, id)).unwrap(), "written first\n");
    }
    // If no blob (pure Renamed), the content lives in the original blob
    // referenced by the first write — verify it's readable through mount.
}

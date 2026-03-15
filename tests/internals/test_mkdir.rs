use crate::helpers::AgfsSession;
use agfs::journal::Record;
use super::helpers::{journal, changes, blob_entries, blob_path, blob_id_for};
use std::fs;

// ── Journal ──────────────────────────────────────────────────────────────────

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

/// Removing a base directory produces a Delete record.
#[test]
fn rmdir_base_dir_produces_delete_record() {
    let s = AgfsSession::new().expect("session setup");

    // subdir/ is seeded in base
    fs::remove_file(s.mnt_path("subdir/deep.txt")).expect("unlink nested file");
    fs::remove_dir(s.mnt_path("subdir")).expect("rmdir base dir");

    let records = journal(&s);
    assert!(
        records.iter().any(|r| matches!(r, Record::Delete { path } if path.ends_with("/subdir"))),
        "journal should have a D record for base dir: {records:?}"
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

// ── Staging ──────────────────────────────────────────────────────────────────

/// mkdir creates an empty directory blob in staging.
#[test]
fn mkdir_creates_directory_blob() {
    let s = AgfsSession::new().expect("session setup");

    fs::create_dir(s.mnt_path("newdir")).expect("mkdir");

    let ch = changes(&s);
    let id = blob_id_for(&ch, "/newdir");
    let blob = blob_path(&s, id);

    assert!(blob.is_dir(), "mkdir blob should be a directory");
    let entries: Vec<_> = fs::read_dir(&blob).unwrap().collect();
    assert!(entries.is_empty(), "mkdir blob should be empty (children get their own blobs)");
}

/// mkdir -p with a file inside: both directory and file get blobs.
#[test]
fn mkdir_with_file_creates_separate_blobs() {
    let s = AgfsSession::new().expect("session setup");

    fs::create_dir_all(s.mnt_path("parent/child")).expect("mkdir -p");
    fs::write(s.mnt_path("parent/child/data.txt"), "nested\n").expect("write");

    let ch = changes(&s);

    // The file should have its own blob
    let file_id = blob_id_for(&ch, "/data.txt");
    assert!(blob_path(&s, file_id).is_file(), "file should have its own blob");
    assert_eq!(fs::read_to_string(blob_path(&s, file_id)).unwrap(), "nested\n");

    // Parent directories should also have blob entries
    let dir_ids: Vec<u64> = ch.iter()
        .filter_map(|c| match c {
            agfs::journal::Change::Added { path, blob_id } if path.ends_with("/parent") || path.ends_with("/child") => Some(*blob_id),
            _ => None,
        })
        .collect();
    for id in &dir_ids {
        assert!(blob_path(&s, *id).is_dir(), "directory blob {id} should be a dir");
    }
}

/// rmdir does NOT create a staging blob (only journal D record).
#[test]
fn rmdir_creates_no_blob() {
    let s = AgfsSession::new().expect("session setup");

    fs::create_dir(s.mnt_path("tmpdir")).expect("mkdir");
    let blobs_before = blob_entries(&s);

    fs::remove_dir(s.mnt_path("tmpdir")).expect("rmdir");
    let blobs_after = blob_entries(&s);

    // rmdir should not add new blobs (the mkdir blob may be cleaned up or kept,
    // but no *new* blob should appear for the delete operation).
    assert!(
        blobs_after.len() <= blobs_before.len(),
        "rmdir should not create new staging blobs: before={blobs_before:?} after={blobs_after:?}"
    );
}

/// Pure directory rename creates no new blob (only journal R record).
#[test]
fn rename_dir_creates_no_blob() {
    let s = AgfsSession::new().expect("session setup");

    fs::create_dir(s.mnt_path("olddir")).expect("mkdir");
    let blobs_after_mkdir = blob_entries(&s);

    fs::rename(s.mnt_path("olddir"), s.mnt_path("newdir")).expect("rename dir");
    let blobs_after_rename = blob_entries(&s);

    assert_eq!(
        blobs_after_mkdir, blobs_after_rename,
        "pure dir rename should not create new staging blobs"
    );
}

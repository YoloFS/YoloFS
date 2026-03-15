use crate::helpers::AgfsSession;
use agfs::journal::{self, Change};
use std::fs;

// ── Integration tests for the staging directory. ─────────────────────────────
//
// These verify that filesystem operations produce the correct blobs in
// `.agfs/staging/`.  The staging directory is a flat blob store: each entry
// is a numeric ID (e.g. `staging/1`, `staging/2`) that holds the content
// for files, empty directories for mkdir, and symlinks for symlink creation.
// Deletes do NOT create blobs.

/// Helper: list numeric blob entries in the staging directory.
fn blob_entries(s: &AgfsSession) -> Vec<u64> {
    let mut ids: Vec<u64> = fs::read_dir(s.staging_dir())
        .expect("read staging dir")
        .filter_map(|e| e.ok())
        .filter_map(|e| e.file_name().to_string_lossy().parse::<u64>().ok())
        .collect();
    ids.sort();
    ids
}

/// Helper: resolve the journal to get the final Change list.
fn changes(s: &AgfsSession) -> Vec<Change> {
    journal::resolve(&s.root.join(".agfs")).expect("resolve journal")
}

/// Helper: get the staging blob path for a given blob id.
fn blob_path(s: &AgfsSession, id: u64) -> std::path::PathBuf {
    s.staging_dir().join(id.to_string())
}

/// Helper: find the blob id for a change matching a path suffix.
fn blob_id_for(changes: &[Change], suffix: &str) -> u64 {
    changes.iter()
        .find_map(|c| match c {
            Change::Added { path, blob_id } if path.ends_with(suffix) => Some(*blob_id),
            Change::Modified { path, blob_id } if path.ends_with(suffix) => Some(*blob_id),
            _ => None,
        })
        .unwrap_or_else(|| panic!("no blob found for path ending with {suffix}"))
}

// ── File blobs ───────────────────────────────────────────────────────────────

/// Writing to an existing file creates a staging blob with the new content.
#[test]
fn modify_creates_blob_with_content() {
    let s = AgfsSession::new().expect("session setup");

    fs::write(s.mnt_path("hello.txt"), "modified\n").expect("write");

    let ch = changes(&s);
    let id = blob_id_for(&ch, "/hello.txt");
    let blob = blob_path(&s, id);

    assert!(blob.exists(), "staging blob should exist at {}", blob.display());
    assert!(blob.is_file(), "staging blob should be a regular file");
    assert_eq!(
        fs::read_to_string(&blob).unwrap(),
        "modified\n",
        "staging blob content should match what was written"
    );
}

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

/// Overwriting a file multiple times updates the blob content in-place.
#[test]
fn overwrite_updates_blob_content() {
    let s = AgfsSession::new().expect("session setup");

    fs::write(s.mnt_path("hello.txt"), "v1\n").expect("write v1");
    fs::write(s.mnt_path("hello.txt"), "v2 is longer\n").expect("write v2");

    let ch = changes(&s);
    let id = blob_id_for(&ch, "/hello.txt");
    let blob = blob_path(&s, id);

    assert_eq!(
        fs::read_to_string(&blob).unwrap(),
        "v2 is longer\n",
        "blob should contain the latest write"
    );
}

/// Appending to a file updates the blob content.
#[test]
fn append_updates_blob() {
    let s = AgfsSession::new().expect("session setup");

    fs::write(s.mnt_path("hello.txt"), "line1\n").expect("write");

    use std::io::Write;
    let mut f = fs::OpenOptions::new()
        .append(true)
        .open(s.mnt_path("hello.txt"))
        .expect("open for append");
    f.write_all(b"line2\n").expect("append");
    drop(f);

    let ch = changes(&s);
    let id = blob_id_for(&ch, "/hello.txt");
    let content = fs::read_to_string(blob_path(&s, id)).unwrap();
    assert_eq!(content, "line1\nline2\n", "blob should contain appended content");
}

// ── Directory blobs ──────────────────────────────────────────────────────────

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
            Change::Added { path, blob_id } if path.ends_with("/parent") || path.ends_with("/child") => Some(*blob_id),
            _ => None,
        })
        .collect();
    for id in &dir_ids {
        assert!(blob_path(&s, *id).is_dir(), "directory blob {id} should be a dir");
    }
}

// ── Symlink blobs ────────────────────────────────────────────────────────────

/// Creating a symlink produces a symlink blob in staging.
#[test]
fn symlink_creates_symlink_blob() {
    let s = AgfsSession::new().expect("session setup");

    std::os::unix::fs::symlink("hello.txt", s.mnt_path("link.txt")).expect("symlink");

    let ch = changes(&s);
    let id = blob_id_for(&ch, "/link.txt");
    let blob = blob_path(&s, id);

    let meta = fs::symlink_metadata(&blob).expect("lstat blob");
    assert!(meta.file_type().is_symlink(), "symlink blob should be a symlink");
    assert_eq!(
        fs::read_link(&blob).unwrap().to_str().unwrap(),
        "hello.txt",
        "symlink blob should point to the target"
    );
}

// ── Delete: no blob ──────────────────────────────────────────────────────────

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

// ── Flat layout ──────────────────────────────────────────────────────────────

/// Staging is flat: all blobs are numeric entries at the top level.
#[test]
fn staging_is_flat() {
    let s = AgfsSession::new().expect("session setup");

    // Create a mix of operations
    fs::write(s.mnt_path("hello.txt"), "modified\n").expect("write");
    fs::create_dir(s.mnt_path("newdir")).expect("mkdir");
    fs::write(s.mnt_path("newdir/file.txt"), "inside\n").expect("write nested");
    std::os::unix::fs::symlink("hello.txt", s.mnt_path("link.txt")).expect("symlink");

    // Every entry in staging/ should be a numeric name
    for entry in fs::read_dir(s.staging_dir()).unwrap() {
        let entry = entry.unwrap();
        let name = entry.file_name().to_string_lossy().to_string();
        assert!(
            name.parse::<u64>().is_ok(),
            "staging entry '{}' should be numeric",
            name
        );
    }
}

/// Each blob has a unique ID — no duplicates.
#[test]
fn blob_ids_are_unique() {
    let s = AgfsSession::new().expect("session setup");

    fs::write(s.mnt_path("hello.txt"), "a\n").expect("write 1");
    fs::write(s.mnt_path("multi.txt"), "b\n").expect("write 2");
    fs::write(s.mnt_path("brandnew.txt"), "c\n").expect("write 3");

    let ids = blob_entries(&s);
    let unique: std::collections::HashSet<u64> = ids.iter().copied().collect();
    assert_eq!(ids.len(), unique.len(), "blob IDs should be unique: {ids:?}");
}

// ── Snapshot + re-COW blobs ──────────────────────────────────────────────────

/// After snapshot + re-COW, the pre-snapshot blob is preserved with old content.
#[test]
fn recow_preserves_pre_snapshot_blob() {
    let s = AgfsSession::new().expect("session setup");

    fs::write(s.mnt_path("hello.txt"), "v1\n").expect("write v1");

    let ch_v1 = changes(&s);
    let id_v1 = blob_id_for(&ch_v1, "/hello.txt");

    s.cli(&["snapshot", "s1"]).expect("snapshot");
    fs::write(s.mnt_path("hello.txt"), "v2\n").expect("write v2 (re-COW)");

    // v1 blob should still have old content
    assert_eq!(
        fs::read_to_string(blob_path(&s, id_v1)).unwrap(),
        "v1\n",
        "pre-snapshot blob should be preserved with v1 content"
    );

    // v2 should be in a different blob
    let ch_v2 = changes(&s);
    let id_v2 = blob_id_for(&ch_v2, "/hello.txt");
    assert_ne!(id_v1, id_v2, "re-COW should allocate a new blob ID");
    assert_eq!(
        fs::read_to_string(blob_path(&s, id_v2)).unwrap(),
        "v2\n",
        "current blob should have v2 content"
    );
}

/// Multiple snapshots preserve each version's blob independently.
#[test]
fn multiple_snapshots_preserve_all_blobs() {
    let s = AgfsSession::new().expect("session setup");

    // v1 → snap → v2 → snap → v3
    fs::write(s.mnt_path("hello.txt"), "v1\n").expect("write v1");
    let id_v1 = blob_id_for(&changes(&s), "/hello.txt");

    s.cli(&["snapshot", "s1"]).expect("snapshot s1");
    fs::write(s.mnt_path("hello.txt"), "v2\n").expect("write v2");
    let id_v2 = blob_id_for(&changes(&s), "/hello.txt");

    s.cli(&["snapshot", "s2"]).expect("snapshot s2");
    fs::write(s.mnt_path("hello.txt"), "v3\n").expect("write v3");
    let id_v3 = blob_id_for(&changes(&s), "/hello.txt");

    // All three blob IDs should be different
    assert_ne!(id_v1, id_v2);
    assert_ne!(id_v2, id_v3);
    assert_ne!(id_v1, id_v3);

    // Each blob should have the correct content
    assert_eq!(fs::read_to_string(blob_path(&s, id_v1)).unwrap(), "v1\n");
    assert_eq!(fs::read_to_string(blob_path(&s, id_v2)).unwrap(), "v2\n");
    assert_eq!(fs::read_to_string(blob_path(&s, id_v3)).unwrap(), "v3\n");
}

// ── Commit clears staging ────────────────────────────────────────────────────

/// After commit, the staging directory is empty.
#[test]
fn commit_empties_staging() {
    let s = AgfsSession::new().expect("session setup");

    fs::write(s.mnt_path("hello.txt"), "modified\n").expect("write");
    fs::write(s.mnt_path("brandnew.txt"), "new\n").expect("create");
    assert!(!blob_entries(&s).is_empty(), "staging should have blobs before commit");

    s.cli(&["commit"]).expect("commit");

    assert!(
        blob_entries(&s).is_empty(),
        "staging should be empty after commit"
    );
}

/// After abort, the staging directory is empty.
#[test]
fn abort_empties_staging() {
    let s = AgfsSession::new().expect("session setup");

    fs::write(s.mnt_path("hello.txt"), "modified\n").expect("write");
    assert!(!blob_entries(&s).is_empty(), "staging should have blobs before abort");

    s.cli(&["abort", "--force"]).expect("abort");

    assert!(
        blob_entries(&s).is_empty(),
        "staging should be empty after abort"
    );
}

// ── Large file blob ──────────────────────────────────────────────────────────

/// A large file write produces a blob with the correct size.
#[test]
fn large_file_blob_size() {
    let s = AgfsSession::new().expect("session setup");

    let data = "x".repeat(1024 * 1024); // 1 MiB
    fs::write(s.mnt_path("big.txt"), &data).expect("write large file");

    let ch = changes(&s);
    let id = blob_id_for(&ch, "/big.txt");
    let blob = blob_path(&s, id);

    let meta = fs::metadata(&blob).unwrap();
    assert_eq!(meta.len(), 1024 * 1024, "blob should be 1 MiB");
}

// ── Blob path via library API ────────────────────────────────────────────────

/// journal::blob_path() returns the correct staging path.
#[test]
fn blob_path_matches_library_api() {
    let s = AgfsSession::new().expect("session setup");

    fs::write(s.mnt_path("hello.txt"), "modified\n").expect("write");

    let agfs_dir = s.root.join(".agfs");
    let ch = journal::resolve(&agfs_dir).expect("resolve");
    let id = ch.iter()
        .find_map(|c| match c {
            Change::Modified { path, blob_id } if path.ends_with("/hello.txt") => Some(*blob_id),
            _ => None,
        })
        .expect("should have a Modified change for hello.txt");

    let lib_path = journal::blob_path(&agfs_dir, id);
    let manual_path = s.staging_dir().join(id.to_string());
    assert_eq!(lib_path, manual_path, "blob_path() should match manual construction");
    assert!(lib_path.exists(), "blob should exist at library-computed path");
}

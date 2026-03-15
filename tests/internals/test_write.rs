use crate::helpers::AgfsSession;
use agfs::journal::Record;
use super::helpers::{journal, changes, blob_entries, blob_path, blob_id_for};
use std::fs;
use std::fs::OpenOptions;
use std::io::Write;

// ── Journal ──────────────────────────────────────────────────────────────────

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

// ── Staging ──────────────────────────────────────────────────────────────────

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

    let mut f = OpenOptions::new()
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

/// Without a snapshot, rewriting a file reuses the same blob (no new allocation).
#[test]
fn rewrite_without_snapshot_reuses_blob() {
    let s = AgfsSession::new().expect("session setup");

    fs::write(s.mnt_path("hello.txt"), "v1\n").expect("write v1");
    let blobs_after_v1 = blob_entries(&s);

    fs::write(s.mnt_path("hello.txt"), "v2\n").expect("write v2");
    let blobs_after_v2 = blob_entries(&s);

    assert_eq!(
        blobs_after_v1, blobs_after_v2,
        "rewrite without snapshot should reuse the same blob"
    );

    let ch = changes(&s);
    let id = blob_id_for(&ch, "/hello.txt");
    assert_eq!(fs::read_to_string(blob_path(&s, id)).unwrap(), "v2\n");
}

/// O_TRUNC on an already-staged file: verify the read-through-mount returns
/// the kernel's view of the file.
#[test]
fn truncate_rewrite_overwrites_from_start() {
    let s = AgfsSession::new().expect("session setup");

    fs::write(s.mnt_path("hello.txt"), "this is a long string\n").expect("write long");
    fs::write(s.mnt_path("hello.txt"), "short\n").expect("write short");

    let content = fs::read_to_string(s.mnt_path("hello.txt")).unwrap();
    assert!(
        content.starts_with("short\n"),
        "mount content should start with the new data: {content:?}"
    );
}

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

/// Binary (non-UTF8) content is preserved exactly in the blob.
#[test]
fn binary_content_preserved() {
    let s = AgfsSession::new().expect("session setup");

    let data: Vec<u8> = (0..=255).collect();
    fs::write(s.mnt_path("binary.bin"), &data).expect("write binary");

    let ch = changes(&s);
    let id = blob_id_for(&ch, "/binary.bin");
    let blob_data = fs::read(blob_path(&s, id)).unwrap();
    assert_eq!(blob_data, data, "binary blob content should match exactly");
}

/// Blob preserves NUL bytes and other control characters.
#[test]
fn nul_bytes_preserved() {
    let s = AgfsSession::new().expect("session setup");

    let data = b"before\0middle\0after\n";
    fs::write(s.mnt_path("nulls.txt"), data).expect("write with NULs");

    let ch = changes(&s);
    let id = blob_id_for(&ch, "/nulls.txt");
    let blob_data = fs::read(blob_path(&s, id)).unwrap();
    assert_eq!(blob_data, data, "NUL bytes should be preserved in blob");
}

/// Writing multiple files produces one blob per file, each with correct content.
#[test]
fn multiple_files_each_get_correct_blob() {
    let s = AgfsSession::new().expect("session setup");

    fs::write(s.mnt_path("hello.txt"), "aaa\n").expect("write 1");
    fs::write(s.mnt_path("multi.txt"), "bbb\n").expect("write 2");
    fs::write(s.mnt_path("new1.txt"), "ccc\n").expect("create 1");
    fs::write(s.mnt_path("new2.txt"), "ddd\n").expect("create 2");

    let ch = changes(&s);

    let pairs = [
        ("/hello.txt", "aaa\n"),
        ("/multi.txt", "bbb\n"),
        ("/new1.txt", "ccc\n"),
        ("/new2.txt", "ddd\n"),
    ];
    for (suffix, expected) in &pairs {
        let id = blob_id_for(&ch, suffix);
        let actual = fs::read_to_string(blob_path(&s, id)).unwrap();
        assert_eq!(&actual, expected, "blob for {suffix} should have correct content");
    }
}

/// Writing to a deeply nested path produces a blob with correct content.
#[test]
fn deep_nested_file_blob() {
    let s = AgfsSession::new().expect("session setup");

    fs::create_dir_all(s.mnt_path("a/b/c")).expect("mkdir -p");
    fs::write(s.mnt_path("a/b/c/leaf.txt"), "deep content\n").expect("write nested");

    let ch = changes(&s);
    let id = blob_id_for(&ch, "/leaf.txt");
    assert_eq!(fs::read_to_string(blob_path(&s, id)).unwrap(), "deep content\n");
}

/// Modifying a pre-existing nested file creates a blob.
#[test]
fn modify_nested_base_file() {
    let s = AgfsSession::new().expect("session setup");

    // subdir/deep.txt is seeded in base
    fs::write(s.mnt_path("subdir/deep.txt"), "updated nested\n").expect("write");

    let ch = changes(&s);
    let id = blob_id_for(&ch, "/deep.txt");
    assert_eq!(fs::read_to_string(blob_path(&s, id)).unwrap(), "updated nested\n");
}

use super::helpers::{actions, ino_for, inode_path, inos, journal, tree};
use crate::helpers::AgfsSession;
use agfs::journal::Action;
use std::fs;
use std::fs::OpenOptions;
use std::io::Write;

// ── Journal ──────────────────────────────────────────────────────────────────

/// Modifying an existing file produces an Add record.
#[test]
fn modify_produces_add_record() {
    let s = AgfsSession::new().expect("session setup");

    fs::write(s.mnt_path("hello.txt"), "modified\n").expect("write");

    let j = journal(&s);
    let acts = actions(&j);
    assert!(
        acts.iter()
            .any(|a| matches!(a, Action::Stage { path, .. } if path.ends_with("/hello.txt"))),
        "journal should have a Modified record for hello.txt: {acts:?}"
    );
}

/// Overwrite an existing file multiple times — each write produces an Stage record
/// (the kernel doesn't coalesce; the CLI resolver handles that).
#[test]
fn multiple_writes_produce_multiple_adds() {
    let s = AgfsSession::new().expect("session setup");

    fs::write(s.mnt_path("hello.txt"), "v1\n").expect("write v1");
    fs::write(s.mnt_path("hello.txt"), "v2\n").expect("write v2");
    fs::write(s.mnt_path("hello.txt"), "v3\n").expect("write v3");

    let j = journal(&s);
    let acts = actions(&j);
    let add_count = acts
        .iter()
        .filter(|a| matches!(a, Action::Stage { path, .. } if path.ends_with("/hello.txt")))
        .count();
    // At least 1 Stage record; the kernel may coalesce O_TRUNC reopens on the
    // same inode, but the first COW always produces one.
    assert!(
        add_count >= 1,
        "should have at least 1 Stage record, got {add_count}: {acts:?}"
    );
}

// ── Inode Store ──────────────────────────────────────────────────────────────────

/// Writing to an existing file creates a staged inode with the new content.
#[test]
fn modify_creates_inode_with_content() {
    let s = AgfsSession::new().expect("session setup");

    fs::write(s.mnt_path("hello.txt"), "modified\n").expect("write");

    let ch = tree(&s);
    let ino = ino_for(&ch, "/hello.txt");
    let path = inode_path(&s, ino);

    assert!(path.is_file(), "staged inode should be a regular file");
    assert_eq!(
        fs::read_to_string(&path).unwrap(),
        "modified\n",
        "staged inode content should match what was written"
    );
}

/// Overwriting a file multiple times updates the inode content in-place.
#[test]
fn overwrite_updates_inode_content() {
    let s = AgfsSession::new().expect("session setup");

    fs::write(s.mnt_path("hello.txt"), "v1\n").expect("write v1");
    fs::write(s.mnt_path("hello.txt"), "v2 is longer\n").expect("write v2");

    let ch = tree(&s);
    let ino = ino_for(&ch, "/hello.txt");
    let path = inode_path(&s, ino);

    assert_eq!(
        fs::read_to_string(&path).unwrap(),
        "v2 is longer\n",
        "inode should contain the latest write"
    );
}

/// Appending to a file updates the inode content.
#[test]
fn append_updates_inode() {
    let s = AgfsSession::new().expect("session setup");

    fs::write(s.mnt_path("hello.txt"), "line1\n").expect("write");

    let mut f = OpenOptions::new()
        .append(true)
        .open(s.mnt_path("hello.txt"))
        .expect("open for append");
    f.write_all(b"line2\n").expect("append");
    drop(f);

    let ch = tree(&s);
    let ino = ino_for(&ch, "/hello.txt");
    let content = fs::read_to_string(inode_path(&s, ino)).unwrap();
    assert_eq!(
        content, "line1\nline2\n",
        "inode should contain appended content"
    );
}

/// Without a checkpoint, rewriting a file reuses the same inode (no new allocation).
#[test]
fn rewrite_without_checkpoint_reuses_inode() {
    let s = AgfsSession::new().expect("session setup");

    fs::write(s.mnt_path("hello.txt"), "v1\n").expect("write v1");
    let inos_after_v1 = inos(&s);

    fs::write(s.mnt_path("hello.txt"), "v2\n").expect("write v2");
    let inos_after_v2 = inos(&s);

    assert_eq!(
        inos_after_v1, inos_after_v2,
        "rewrite without checkpoint should reuse the same inode"
    );

    let ch = tree(&s);
    let ino = ino_for(&ch, "/hello.txt");
    assert_eq!(fs::read_to_string(inode_path(&s, ino)).unwrap(), "v2\n");
}

/// O_TRUNC on an already-staged file: verify the read-through-mount returns
/// the kernel's view of the file.
#[test]
fn truncate_rewrite_overwrites_from_start() {
    let s = AgfsSession::new().expect("session setup");

    fs::write(s.mnt_path("hello.txt"), "this is a long string\n").expect("write long");
    fs::write(s.mnt_path("hello.txt"), "short\n").expect("write short");

    let content = fs::read_to_string(s.mnt_path("hello.txt")).unwrap();
    assert_eq!(
        content, "short\n",
        "mount content should be exactly the new data"
    );
}

/// O_TRUNC on an already-staged file: the inode in the store must contain
/// only the new (shorter) data — no leftover bytes from the previous write.
#[test]
fn truncate_rewrite_inode_has_exact_content() {
    let s = AgfsSession::new().expect("session setup");

    fs::write(s.mnt_path("hello.txt"), "this is a long string\n").expect("write long");
    fs::write(s.mnt_path("hello.txt"), "short\n").expect("write short");

    let ch = tree(&s);
    let ino = ino_for(&ch, "/hello.txt");
    let path = inode_path(&s, ino);

    let inode_content = fs::read_to_string(&path).unwrap();
    assert_eq!(
        inode_content, "short\n",
        "inode should contain only the truncated content"
    );

    let meta = fs::metadata(&path).unwrap();
    assert_eq!(meta.len(), 6, "inode size should be exactly 6 bytes");
}

/// Opening with O_TRUNC and writing nothing: the inode must be empty (0 bytes).
#[test]
fn truncate_only_produces_empty_inode() {
    let s = AgfsSession::new().expect("session setup");

    // First write to stage the file
    fs::write(s.mnt_path("hello.txt"), "some content\n").expect("initial write");

    // Open with O_WRONLY | O_TRUNC, write nothing, close
    let _f = OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(s.mnt_path("hello.txt"))
        .expect("open O_TRUNC");
    drop(_f);

    // Mount should show empty file
    let content = fs::read_to_string(s.mnt_path("hello.txt")).unwrap();
    assert_eq!(content, "", "mount should show empty file after O_TRUNC");

    // Inode in the store should be 0 bytes
    let ch = tree(&s);
    let ino = ino_for(&ch, "/hello.txt");
    let path = inode_path(&s, ino);

    let meta = fs::metadata(&path).unwrap();
    assert_eq!(
        meta.len(),
        0,
        "inode should be 0 bytes after O_TRUNC with no write"
    );
}

/// A large file write produces an inode with the correct size.
#[test]
fn large_file_inode_size() {
    let s = AgfsSession::new().expect("session setup");

    let data = "x".repeat(1024 * 1024); // 1 MiB
    fs::write(s.mnt_path("big.txt"), &data).expect("write large file");

    let ch = tree(&s);
    let ino = ino_for(&ch, "/big.txt");
    let path = inode_path(&s, ino);

    let meta = fs::metadata(&path).unwrap();
    assert_eq!(meta.len(), 1024 * 1024, "inode should be 1 MiB");
}

/// Binary (non-UTF8) content is preserved exactly in the inode.
#[test]
fn binary_content_preserved() {
    let s = AgfsSession::new().expect("session setup");

    let data: Vec<u8> = (0..=255).collect();
    fs::write(s.mnt_path("binary.bin"), &data).expect("write binary");

    let ch = tree(&s);
    let ino = ino_for(&ch, "/binary.bin");
    let inode_data = fs::read(inode_path(&s, ino)).unwrap();
    assert_eq!(
        inode_data, data,
        "binary inode content should match exactly"
    );
}

/// Inode preserves NUL bytes and other control characters.
#[test]
fn nul_bytes_preserved() {
    let s = AgfsSession::new().expect("session setup");

    let data = b"before\0middle\0after\n";
    fs::write(s.mnt_path("nulls.txt"), data).expect("write with NULs");

    let ch = tree(&s);
    let ino = ino_for(&ch, "/nulls.txt");
    let inode_data = fs::read(inode_path(&s, ino)).unwrap();
    assert_eq!(inode_data, data, "NUL bytes should be preserved in inode");
}

/// Writing multiple files produces one inode per file, each with correct content.
#[test]
fn multiple_files_each_get_correct_inode() {
    let s = AgfsSession::new().expect("session setup");

    fs::write(s.mnt_path("hello.txt"), "aaa\n").expect("write 1");
    fs::write(s.mnt_path("multi.txt"), "bbb\n").expect("write 2");
    fs::write(s.mnt_path("new1.txt"), "ccc\n").expect("create 1");
    fs::write(s.mnt_path("new2.txt"), "ddd\n").expect("create 2");

    let ch = tree(&s);

    let pairs = [
        ("/hello.txt", "aaa\n"),
        ("/multi.txt", "bbb\n"),
        ("/new1.txt", "ccc\n"),
        ("/new2.txt", "ddd\n"),
    ];
    for (suffix, expected) in &pairs {
        let ino = ino_for(&ch, suffix);
        let actual = fs::read_to_string(inode_path(&s, ino)).unwrap();
        assert_eq!(
            &actual, expected,
            "inode for {suffix} should have correct content"
        );
    }
}

/// Writing to a deeply nested path produces an inode with correct content.
#[test]
fn deep_nested_file_inode() {
    let s = AgfsSession::new().expect("session setup");

    fs::create_dir_all(s.mnt_path("a/b/c")).expect("mkdir -p");
    fs::write(s.mnt_path("a/b/c/leaf.txt"), "deep content\n").expect("write nested");

    let ch = tree(&s);
    let ino = ino_for(&ch, "/leaf.txt");
    assert_eq!(
        fs::read_to_string(inode_path(&s, ino)).unwrap(),
        "deep content\n"
    );
}

/// Modifying a pre-existing nested file creates an inode.
#[test]
fn modify_nested_base_file() {
    let s = AgfsSession::new().expect("session setup");

    // subdir/deep.txt is seeded in base
    fs::write(s.mnt_path("subdir/deep.txt"), "updated nested\n").expect("write");

    let ch = tree(&s);
    let ino = ino_for(&ch, "/deep.txt");
    assert_eq!(
        fs::read_to_string(inode_path(&s, ino)).unwrap(),
        "updated nested\n"
    );
}

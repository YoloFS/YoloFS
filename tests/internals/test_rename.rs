use super::helpers::{changes, ino_for, inode_path, inos, journal};
use crate::helpers::AgfsSession;
use agfs::journal::Record;
use std::fs;

// ── Journal ──────────────────────────────────────────────────────────────────

/// Renaming a file produces a Redirect record with dtype=File.
#[test]
fn rename_produces_rename_record() {
    let s = AgfsSession::new().expect("session setup");

    fs::rename(s.mnt_path("hello.txt"), s.mnt_path("moved.txt")).expect("rename");

    let records = journal(&s);
    assert!(
        records.0
            .iter()
            .any(|r| matches!(r, Record::Redirect { old, new, dtype: Some(agfs::journal::DType::File), .. }
            if new.ends_with("/moved.txt") && old.ends_with("/hello.txt"))),
        "journal should have a Redirect(dtype=File) record for hello.txt → moved.txt: {records:?}"
    );
}

// ── Inode Store ──────────────────────────────────────────────────────────────────

/// Pure rename of a base file creates no new inode (only journal R record).
#[test]
fn pure_rename_creates_no_inode() {
    let s = AgfsSession::new().expect("session setup");

    let inos_before = inos(&s);
    fs::rename(s.mnt_path("hello.txt"), s.mnt_path("moved.txt")).expect("rename");
    let inos_after = inos(&s);

    assert_eq!(
        inos_before, inos_after,
        "pure rename should not create new staged inodes"
    );
}

/// Rename + modify (write after rename) produces an inode at the new path.
#[test]
fn rename_then_write_produces_inode() {
    let s = AgfsSession::new().expect("session setup");

    fs::rename(s.mnt_path("hello.txt"), s.mnt_path("moved.txt")).expect("rename");
    fs::write(s.mnt_path("moved.txt"), "new content\n").expect("write renamed file");

    let ch = changes(&s);
    let ino = ino_for(&ch, "/moved.txt");

    assert_eq!(
        fs::read_to_string(inode_path(&s, ino)).unwrap(),
        "new content\n"
    );
}

/// Write then rename: inode content still correct under the new path.
#[test]
fn write_then_rename_inode_content() {
    let s = AgfsSession::new().expect("session setup");

    fs::write(s.mnt_path("hello.txt"), "written first\n").expect("write");
    fs::rename(s.mnt_path("hello.txt"), s.mnt_path("final.txt")).expect("rename");

    // Read through mount to verify
    let content = fs::read_to_string(s.mnt_path("final.txt")).unwrap();
    assert_eq!(content, "written first\n");

    // The inode should have the written content — resolves as Renamed + Modified.
    let ch = changes(&s);
    let ino = ino_for(&ch, "/final.txt");
    assert_eq!(
        fs::read_to_string(inode_path(&s, ino)).unwrap(),
        "written first\n"
    );
}

/// Rename chain: a→b→c. Journal should show two Redirect records.
/// Each carries the dentry path as old (no chain resolution in kernel).
#[test]
fn rename_chain_journal_records() {
    let s = AgfsSession::new().expect("session setup");

    fs::rename(s.mnt_path("hello.txt"), s.mnt_path("step1.txt")).expect("a→b");
    fs::rename(s.mnt_path("step1.txt"), s.mnt_path("step2.txt")).expect("b→c");

    let records = journal(&s);

    // Both renames should produce Redirect records (base-file renames)
    let redirects: Vec<_> = records.0
        .iter()
        .filter(|r| matches!(r, Record::Redirect { .. }))
        .collect();
    assert_eq!(
        redirects.len(),
        2,
        "chain should produce 2 Redirect records: {records:?}"
    );
}

/// Rename onto an existing base file (overwrite): the old target
/// is implicitly replaced. No explicit delete record for the target
/// — the rename dirent overwrites whatever was there.
#[test]
fn rename_overwrite_journal() {
    let s = AgfsSession::new().expect("session setup");

    // hello.txt overwrites subdir/deep.txt
    fs::rename(s.mnt_path("hello.txt"), s.mnt_path("subdir/deep.txt")).expect("overwrite");

    let records = journal(&s);

    // Should have Replace(hello.txt → subdir/deep.txt) as a single record
    // (no separate Delete for hello.txt — fused into the R/P record).
    let has_replace = records.0.iter().any(|r| {
        matches!(r, Record::Replace { old, new, .. }
        if new.ends_with("/deep.txt") && old.ends_with("/hello.txt"))
    });
    assert!(
        has_replace,
        "should have Replace for hello.txt → deep.txt: {records:?}"
    );
}

/// Rename back and forth: a→b→a. After resolution, no staged changes
/// should remain (the rename cancels out).
#[test]
fn rename_back_and_forth_no_changes() {
    let s = AgfsSession::new().expect("session setup");

    fs::rename(s.mnt_path("hello.txt"), s.mnt_path("temp.txt")).expect("a→b");
    fs::rename(s.mnt_path("temp.txt"), s.mnt_path("hello.txt")).expect("b→a");

    let ch = changes(&s);
    assert!(
        ch.is_empty(),
        "rename back and forth should produce no changes, got: {ch:?}"
    );
}

/// Rename a staged (newly created) file to overwrite a base file.
/// The destination exists in base, so the kernel emits D + M (not D + A).
#[test]
fn rename_staged_file_to_base_path() {
    let s = AgfsSession::new().expect("session setup");

    // Create a new staged file
    fs::write(s.mnt_path("brand_new.txt"), "staged content\n").expect("create");
    // Rename it to overwrite multi.txt (which exists in base)
    fs::rename(s.mnt_path("brand_new.txt"), s.mnt_path("multi.txt")).expect("rename");

    let records = journal(&s);

    // Should have Delete for brand_new.txt
    let has_delete = records.0
        .iter()
        .any(|r| matches!(r, Record::Deleted { path } if path.ends_with("/brand_new.txt")));
    assert!(
        has_delete,
        "should have Delete for brand_new.txt: {records:?}"
    );
    // Destination exists in base → kernel emits Modified (not Added)
    let has_modified = records.0
        .iter()
        .any(|r| matches!(r, Record::Modified { path, .. } if path.ends_with("/multi.txt")));
    assert!(
        has_modified,
        "should have Modified for multi.txt (dest in base): {records:?}"
    );
    // Verify the file content is correct
    let content = fs::read_to_string(s.mnt_path("multi.txt")).expect("read");
    assert_eq!(content, "staged content\n");
}

use super::helpers::{actions, ino_for, inode_path, inos, journal, tree};
use crate::helpers::YoloSession;
use std::fs;
use yolofs::journal::Action;

// ── Journal ──────────────────────────────────────────────────────────────────

/// Creating a directory produces an Entry record with dtype=Dir.
#[test]
fn mkdir_produces_add_record() {
    let s = YoloSession::new().expect("session setup");

    fs::create_dir(s.mnt_path("newdir")).expect("mkdir");

    let j = journal(&s);
    let acts = actions(&j);
    assert!(
        acts.iter()
            .any(|a| matches!(a, Action::Stage { path, .. } if path.ends_with("/newdir"))),
        "journal should have a Stage record for newdir: {acts:?}"
    );
}

/// Removing a directory produces a Delete record.
#[test]
fn rmdir_produces_delete_record() {
    let s = YoloSession::new().expect("session setup");

    // Create through mount then remove
    fs::create_dir(s.mnt_path("tmpdir")).expect("mkdir");
    fs::remove_dir(s.mnt_path("tmpdir")).expect("rmdir");

    let j = journal(&s);
    let acts = actions(&j);
    assert!(
        acts.iter()
            .any(|a| matches!(a, Action::Delete { path, .. } if path.ends_with("/tmpdir"))),
        "journal should have a Deleted record for tmpdir: {acts:?}"
    );
}

/// Removing a base directory produces a Delete record.
#[test]
fn rmdir_base_dir_produces_delete_record() {
    let s = YoloSession::new().expect("session setup");

    // subdir/ is seeded in base
    fs::remove_file(s.mnt_path("subdir/deep.txt")).expect("unlink nested file");
    fs::remove_dir(s.mnt_path("subdir")).expect("rmdir base dir");

    let j = journal(&s);
    let acts = actions(&j);
    assert!(
        acts.iter()
            .any(|a| matches!(a, Action::Delete { path, .. } if path.ends_with("/subdir"))),
        "journal should have a Deleted record for base dir: {acts:?}"
    );
}

/// Renaming a staged directory produces a single R record.
#[test]
fn rename_dir_produces_rename_record() {
    let s = YoloSession::new().expect("session setup");

    fs::create_dir(s.mnt_path("olddir")).expect("mkdir");
    fs::rename(s.mnt_path("olddir"), s.mnt_path("newdir")).expect("rename dir");

    let j = journal(&s);
    let acts = actions(&j);
    assert!(
        acts.iter()
            .any(|a| matches!(a, Action::Rename { dst, src, .. }
            if dst.ends_with("/newdir") && src.ends_with("/olddir"))),
        "journal should have a Redirect record for olddir → newdir: {acts:?}"
    );
}

// ── Inode Store ──────────────────────────────────────────────────────────────────

/// mkdir creates an empty directory inode in inode store.
#[test]
fn mkdir_creates_directory_inode() {
    let s = YoloSession::new().expect("session setup");

    fs::create_dir(s.mnt_path("newdir")).expect("mkdir");

    let ch = tree(&s);
    let ino = ino_for(&ch, "/newdir");
    let path = inode_path(&s, ino);

    assert!(path.is_dir(), "mkdir inode should be a directory");
    let entries: Vec<_> = fs::read_dir(&path).unwrap().collect();
    assert!(
        entries.is_empty(),
        "mkdir inode should be empty (children get their own inodes)"
    );
}

/// mkdir -p with a file inside: both directory and file get inodes.
#[test]
fn mkdir_with_file_creates_separate_inodes() {
    let s = YoloSession::new().expect("session setup");

    fs::create_dir_all(s.mnt_path("parent/child")).expect("mkdir -p");
    fs::write(s.mnt_path("parent/child/data.txt"), "nested\n").expect("write");

    let ch = tree(&s);

    // The file should have its own inode
    let file_ino = ino_for(&ch, "/data.txt");
    assert!(
        inode_path(&s, file_ino).is_file(),
        "file should have its own inode"
    );
    assert_eq!(
        fs::read_to_string(inode_path(&s, file_ino)).unwrap(),
        "nested\n"
    );

    // Parent directories should also have inode entries
    let parent_ino = ino_for(&ch, "/parent");
    assert!(
        inode_path(&s, parent_ino).is_dir(),
        "directory inode {parent_ino} should be a dir"
    );
    let child_ino = ino_for(&ch, "/child");
    assert!(
        inode_path(&s, child_ino).is_dir(),
        "directory inode {child_ino} should be a dir"
    );
}

/// rmdir does NOT create a staged inode (only journal DEL record).
#[test]
fn rmdir_creates_no_inode() {
    let s = YoloSession::new().expect("session setup");

    fs::create_dir(s.mnt_path("tmpdir")).expect("mkdir");
    let inos_before = inos(&s);

    fs::remove_dir(s.mnt_path("tmpdir")).expect("rmdir");
    let inos_after = inos(&s);

    // rmdir should not add new inodes (the mkdir inode may be cleaned up or kept,
    // but no *new* inode should appear for the delete operation).
    assert!(
        inos_after.len() <= inos_before.len(),
        "rmdir should not create new staged inodes: before={inos_before:?} after={inos_after:?}"
    );
}

/// Pure directory rename creates no new inode (only journal RDR record).
#[test]
fn rename_dir_creates_no_inode() {
    let s = YoloSession::new().expect("session setup");

    fs::create_dir(s.mnt_path("olddir")).expect("mkdir");
    let inos_after_mkdir = inos(&s);

    fs::rename(s.mnt_path("olddir"), s.mnt_path("newdir")).expect("rename dir");
    let inos_after_rename = inos(&s);

    assert_eq!(
        inos_after_mkdir, inos_after_rename,
        "pure dir rename should not create new staged inodes"
    );
}

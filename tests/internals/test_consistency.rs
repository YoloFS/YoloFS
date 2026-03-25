//! Kernel ↔ CLI directory consistency tests.
//!
//! Verify that the kernel's in-memory directory state (what readdir and
//! lookup expose) matches the CLI's journal-reconstructed DirTree after
//! various operation sequences.
//!
//! Each test:
//!   1. Performs filesystem operations through the mount (kernel handles live)
//!   2. Reads the journal and reconstructs a DirTree (CLI replay logic)
//!   3. Asserts the two views agree on visibility and readdir contents

use super::helpers::{inode_path, tree};
use crate::helpers::AgfsSession;
use agfs::journal::tree::DirNode;
use agfs::journal::{DirTree, Target};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::os::unix::fs::DirEntryExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

// ── Helpers ──────────────────────────────────────────────────────────

/// True if a path is visible in the filesystem (works for symlinks too).
fn path_visible(p: &Path) -> bool {
    p.symlink_metadata().is_ok()
}

/// Collect entry names from a directory listing (non-recursive).
fn entry_names(dir: &Path, skip: &[&str]) -> BTreeSet<String> {
    let Ok(rd) = fs::read_dir(dir) else {
        return BTreeSet::new();
    };
    rd.filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|n| !skip.contains(&n.as_str()))
        .collect()
}

fn root_prefix(s: &AgfsSession) -> String {
    s.root.to_str().unwrap().to_string()
}

fn expected_backing_metadata(s: &AgfsSession, target: &Target) -> std::fs::Metadata {
    match target {
        Target::Inode(ino) => inode_path(s, *ino)
            .symlink_metadata()
            .unwrap_or_else(|e| panic!("staged inode {ino} should exist: {e}")),
        Target::Path(Some(src)) => Path::new(src)
            .symlink_metadata()
            .unwrap_or_else(|e| panic!("redirect source '{src}' should exist: {e}")),
        Target::Path(None) => panic!("passthrough dentry should not be visible"),
        Target::None => panic!("negative dentry has no backing metadata"),
    }
}

fn assert_same_file_type(rel: &str, actual: &std::fs::Metadata, expected: &std::fs::Metadata) {
    assert_eq!(
        actual.is_file(),
        expected.is_file(),
        "file-type mismatch at '/{rel}': expected file={}, got file={}",
        expected.is_file(),
        actual.is_file()
    );
    assert_eq!(
        actual.is_dir(),
        expected.is_dir(),
        "file-type mismatch at '/{rel}': expected dir={}, got dir={}",
        expected.is_dir(),
        actual.is_dir()
    );
    assert_eq!(
        actual.is_symlink(),
        expected.is_symlink(),
        "file-type mismatch at '/{rel}': expected symlink={}, got symlink={}",
        expected.is_symlink(),
        actual.is_symlink()
    );
}

/// Assert that every CLI overlay entry has the correct mount visibility:
///   - Inode/redirect entries must be visible through the mount
///   - Tombstone entries must NOT be visible through the mount
///
/// For visible entries, also verifies the kernel-exposed file type matches
/// the actual backing inode or redirect source.
fn assert_overlay_visible(s: &AgfsSession) {
    let prefix = root_prefix(s);
    let t = tree(s);
    t.for_each(|path, target| {
        let rel = path.strip_prefix(&prefix).unwrap();
        let rel = rel.strip_prefix('/').unwrap_or(rel);
        let mnt = s.mnt_path(rel);
        match target {
            Target::Inode(ino) => {
                let meta = mnt
                    .symlink_metadata()
                    .unwrap_or_else(|e| panic!("overlay inode at '/{rel}' should be visible: {e}"));
                let expected = expected_backing_metadata(s, target);
                assert_same_file_type(rel, &meta, &expected);
                if expected.is_file() {
                    let ipath = inode_path(s, *ino);
                    let ino_content = fs::read(&ipath).unwrap_or_else(|e| {
                        panic!("inode {ino} at '/{rel}' should be readable: {e}")
                    });
                    let mnt_content = fs::read(&mnt).unwrap();
                    assert_eq!(
                        ino_content, mnt_content,
                        "content mismatch at '/{rel}': inode store ≠ mount"
                    );
                }
            }
            Target::Path(Some(_)) => {
                let meta = mnt.symlink_metadata().unwrap_or_else(|e| {
                    panic!("overlay redirect at '/{rel}' should be visible: {e}")
                });
                let expected = expected_backing_metadata(s, target);
                assert_same_file_type(rel, &meta, &expected);
            }
            Target::Path(None) => unreachable!("passthrough dentries are skipped by for_each"),
            Target::None => {
                assert!(
                    !path_visible(&mnt),
                    "negative dentry at '/{rel}' should not be visible"
                );
            }
        }
    });
}

/// Resolve the base filesystem directory for a given relative path,
/// following any Links in the CLI DirTree (handles renamed base dirs).
fn resolve_base_dir(s: &AgfsSession, rel_dir: &str, cli: &DirTree) -> PathBuf {
    if rel_dir.is_empty() {
        return s.root.clone();
    }
    let prefix = root_prefix(s);
    let mut base = s.root.clone();
    let mut tree_path = prefix.clone();

    for component in rel_dir.split('/') {
        tree_path = format!("{tree_path}/{component}");
        let link_target = cli.get_node(&tree_path).and_then(|node| {
            if let DirNode::Dir(d, _) = node {
                if let Target::Path(Some(src)) = d {
                    Some(src.as_str())
                } else {
                    None
                }
            } else {
                None
            }
        });
        base = match link_target {
            Some(target) => PathBuf::from(&target),
            None => base.join(component),
        };
    }
    base
}

/// Assert that readdir of a specific directory through the mount matches
/// the expected set: base entries (not overridden) ∪ staged Inode/Link
/// entries.
///
/// Handles renamed base directories by resolving Links to find the
/// effective base directory.
fn assert_dir_matches(s: &AgfsSession, rel_dir: &str) {
    let prefix = root_prefix(s);
    let dir_prefix = if rel_dir.is_empty() {
        prefix.clone()
    } else {
        format!("{prefix}/{rel_dir}")
    };
    let skip: &[&str] = if rel_dir.is_empty() {
        &[".agfs", "agfs.toml"]
    } else {
        &[]
    };

    let mnt_names = entry_names(&s.mnt_path(rel_dir), skip);

    let t = tree(s);
    let effective_base = resolve_base_dir(s, rel_dir, &t);
    let base_names = entry_names(&effective_base, skip);

    // Overlay entries that are direct children of this directory
    let mut staged: BTreeMap<String, Target> = BTreeMap::new();
    t.for_each(|path, target| {
        if let Some(rest) = path.strip_prefix(&dir_prefix) {
            let rest = rest.strip_prefix('/').unwrap_or(rest);
            if !rest.is_empty() && !rest.contains('/') {
                staged.insert(rest.to_string(), target.clone());
            }
        }
    });

    // expected = base (not overridden) ∪ staged (non-tombstone)
    let mut expected = BTreeSet::new();
    for name in &base_names {
        if !staged.contains_key(name) {
            expected.insert(name.clone());
        }
    }
    for (name, target) in &staged {
        if !matches!(target, Target::None) {
            expected.insert(name.clone());
        }
    }

    let label = if rel_dir.is_empty() {
        "<root>"
    } else {
        rel_dir
    };
    assert_eq!(
        mnt_names,
        expected,
        "'{label}' readdir mismatch\n\
         in mount not expected: {:?}\n\
         expected not in mount: {:?}\n\
         staged: {staged:?}",
        mnt_names.difference(&expected).collect::<Vec<_>>(),
        expected.difference(&mnt_names).collect::<Vec<_>>(),
    );
}

/// Combined assertion: overlay visibility + root directory readdir.
fn assert_consistent(s: &AgfsSession) {
    assert_overlay_visible(s);
    assert_dir_matches(s, "");
}

// ── Add (A) ──────────────────────────────────────────────────────────

#[test]
fn add_file() {
    let s = AgfsSession::new().expect("session setup");
    fs::write(s.mnt_path("new.txt"), "data\n").expect("create");
    assert_consistent(&s);
}

#[test]
fn add_nested_file() {
    let s = AgfsSession::new().expect("session setup");
    fs::create_dir(s.mnt_path("d")).expect("mkdir");
    fs::write(s.mnt_path("d/nested.txt"), "deep\n").expect("create");
    assert_overlay_visible(&s);
    assert_dir_matches(&s, "");
    assert_dir_matches(&s, "d");
}

#[test]
fn add_multiple_files() {
    let s = AgfsSession::new().expect("session setup");
    for i in 0..10 {
        fs::write(s.mnt_path(&format!("f{i}.txt")), format!("data {i}\n")).expect("create");
    }
    assert_consistent(&s);
}

// ── COW (A) ──────────────────────────────────────────────────────────

#[test]
fn modify_base_file() {
    let s = AgfsSession::new().expect("session setup");
    fs::write(s.mnt_path("hello.txt"), "modified\n").expect("write");
    assert_consistent(&s);
}

// ── Delete (D) ───────────────────────────────────────────────────────

/// A+D on a staged-only file produces a tombstone (always-tombstone rule).
#[test]
fn delete_staged_file_tombstones() {
    let s = AgfsSession::new().expect("session setup");
    fs::write(s.mnt_path("temp.txt"), "temp\n").expect("create");
    fs::remove_file(s.mnt_path("temp.txt")).expect("delete");
    assert_consistent(&s);

    // CLI should have a tombstone (Target::None)
    let t = tree(&s);
    assert!(
        t.any(|p, e| p.ends_with("/temp.txt") && matches!(e, Target::None)),
        "A+D on staged-only should produce tombstone: {t:?}"
    );
}

/// D on a base file produces a tombstone.
#[test]
fn delete_base_file_tombstones() {
    let s = AgfsSession::new().expect("session setup");
    fs::remove_file(s.mnt_path("hello.txt")).expect("delete");
    assert_consistent(&s);

    let t = tree(&s);
    assert!(
        t.any(|p, e| p.ends_with("/hello.txt") && matches!(e, Target::None)),
        "D on base file should produce negative dentry: {t:?}"
    );
}

/// Delete all base files — everything tombstoned, mount shows nothing.
#[test]
fn delete_all_base_files() {
    let s = AgfsSession::new().expect("session setup");
    fs::remove_file(s.mnt_path("hello.txt")).expect("delete");
    fs::remove_file(s.mnt_path("multi.txt")).expect("delete");
    fs::remove_file(s.mnt_path("test.sh")).expect("delete");
    fs::remove_file(s.mnt_path("subdir/deep.txt")).expect("delete");
    fs::remove_dir(s.mnt_path("subdir")).expect("rmdir");
    assert_overlay_visible(&s);
    assert_dir_matches(&s, "");
}

// ── Rename (R) ───────────────────────────────────────────────────────

/// Rename a base file: Link at dest, Tombstone at source.
#[test]
fn rename_base_file() {
    let s = AgfsSession::new().expect("session setup");
    fs::rename(s.mnt_path("hello.txt"), s.mnt_path("moved.txt")).expect("rename");
    assert_consistent(&s);
    assert_eq!(
        fs::read_to_string(s.mnt_path("moved.txt")).unwrap(),
        "base content\n"
    );
}

/// Rename a staged file: inode reference moves, tombstone at source.
#[test]
fn rename_staged_file() {
    let s = AgfsSession::new().expect("session setup");
    fs::write(s.mnt_path("new.txt"), "staged\n").expect("create");
    fs::rename(s.mnt_path("new.txt"), s.mnt_path("renamed.txt")).expect("rename");
    assert_consistent(&s);
    assert_eq!(
        fs::read_to_string(s.mnt_path("renamed.txt")).unwrap(),
        "staged\n"
    );
}

/// Chain renames: a→b→c.
#[test]
fn rename_chain() {
    let s = AgfsSession::new().expect("session setup");
    fs::write(s.mnt_path("a.txt"), "start\n").expect("create");
    fs::rename(s.mnt_path("a.txt"), s.mnt_path("b.txt")).expect("rename 1");
    fs::rename(s.mnt_path("b.txt"), s.mnt_path("c.txt")).expect("rename 2");
    assert_consistent(&s);
    assert_eq!(fs::read_to_string(s.mnt_path("c.txt")).unwrap(), "start\n");
}

/// Rename a base directory: Link at dest dir, contents accessible.
#[test]
fn rename_base_dir() {
    let s = AgfsSession::new().expect("session setup");
    fs::rename(s.mnt_path("subdir"), s.mnt_path("newdir")).expect("rename dir");
    assert_overlay_visible(&s);
    assert_dir_matches(&s, "");
    assert_dir_matches(&s, "newdir");

    // Content accessible under new name
    assert_eq!(
        fs::read_to_string(s.mnt_path("newdir/deep.txt")).unwrap(),
        "nested\n"
    );
}

/// Rename across directories: file moves from root to a new staged dir.
#[test]
fn rename_into_new_dir() {
    let s = AgfsSession::new().expect("session setup");
    fs::create_dir(s.mnt_path("target")).expect("mkdir");
    fs::write(s.mnt_path("src.txt"), "moved\n").expect("create");
    fs::rename(s.mnt_path("src.txt"), s.mnt_path("target/dst.txt")).expect("rename cross-dir");
    assert_overlay_visible(&s);
    assert_dir_matches(&s, "");
    assert_dir_matches(&s, "target");
}

/// Rename a base file into a new staged directory.
#[test]
fn rename_base_into_new_dir() {
    let s = AgfsSession::new().expect("session setup");
    fs::create_dir(s.mnt_path("dest")).expect("mkdir");
    fs::rename(s.mnt_path("hello.txt"), s.mnt_path("dest/moved.txt")).expect("rename");
    assert_overlay_visible(&s);
    assert_dir_matches(&s, "");
    assert_dir_matches(&s, "dest");
    assert_eq!(
        fs::read_to_string(s.mnt_path("dest/moved.txt")).unwrap(),
        "base content\n"
    );
}

// ── Rename overwrite (R) ─────────────────────────────────────────────

/// Rename a staged file onto an existing base file (overwrite).
#[test]
fn replace_base_file() {
    let s = AgfsSession::new().expect("session setup");
    fs::write(s.mnt_path("new.txt"), "overwrite\n").expect("create");
    fs::rename(s.mnt_path("new.txt"), s.mnt_path("hello.txt")).expect("replace");
    assert_consistent(&s);
    assert_eq!(
        fs::read_to_string(s.mnt_path("hello.txt")).unwrap(),
        "overwrite\n"
    );
}

// ── mkdir / rmdir ────────────────────────────────────────────────────

#[test]
fn mkdir() {
    let s = AgfsSession::new().expect("session setup");
    fs::create_dir(s.mnt_path("newdir")).expect("mkdir");
    assert_consistent(&s);
}

#[test]
fn mkdir_with_files() {
    let s = AgfsSession::new().expect("session setup");
    fs::create_dir(s.mnt_path("d")).expect("mkdir");
    fs::write(s.mnt_path("d/a.txt"), "a\n").expect("write a");
    fs::write(s.mnt_path("d/b.txt"), "b\n").expect("write b");
    assert_overlay_visible(&s);
    assert_dir_matches(&s, "");
    assert_dir_matches(&s, "d");
}

#[test]
fn rmdir_base() {
    let s = AgfsSession::new().expect("session setup");
    fs::remove_file(s.mnt_path("subdir/deep.txt")).expect("unlink");
    fs::remove_dir(s.mnt_path("subdir")).expect("rmdir");
    assert_consistent(&s);
}

// ── Symlink ──────────────────────────────────────────────────────────

#[test]
fn symlink() {
    let s = AgfsSession::new().expect("session setup");
    std::os::unix::fs::symlink("hello.txt", s.mnt_path("link.txt")).expect("symlink");
    assert_consistent(&s);
}

// ── Complex sequences ────────────────────────────────────────────────

#[test]
fn mixed_operations() {
    let s = AgfsSession::new().expect("session setup");

    fs::write(s.mnt_path("added.txt"), "new\n").expect("create");
    fs::create_dir(s.mnt_path("d")).expect("mkdir");
    fs::write(s.mnt_path("d/x.txt"), "x\n").expect("create nested");
    fs::write(s.mnt_path("multi.txt"), "modified\n").expect("modify base");
    fs::remove_file(s.mnt_path("hello.txt")).expect("delete base");
    fs::rename(s.mnt_path("added.txt"), s.mnt_path("moved.txt")).expect("rename staged");

    assert_overlay_visible(&s);
    assert_dir_matches(&s, "");
    assert_dir_matches(&s, "d");
}

#[test]
fn create_delete_recreate() {
    let s = AgfsSession::new().expect("session setup");
    fs::write(s.mnt_path("cycle.txt"), "v1\n").expect("create v1");
    fs::remove_file(s.mnt_path("cycle.txt")).expect("delete");
    fs::write(s.mnt_path("cycle.txt"), "v2\n").expect("create v2");
    assert_consistent(&s);
    assert_eq!(fs::read_to_string(s.mnt_path("cycle.txt")).unwrap(), "v2\n");
}

/// Modify a base file then delete it (A+D → tombstone).
#[test]
fn modify_then_delete_base() {
    let s = AgfsSession::new().expect("session setup");
    fs::write(s.mnt_path("hello.txt"), "modified\n").expect("modify");
    fs::remove_file(s.mnt_path("hello.txt")).expect("delete");
    assert_consistent(&s);

    let t = tree(&s);
    assert!(
        t.any(|p, e| p.ends_with("/hello.txt") && matches!(e, Target::None)),
        "A+D on base should produce negative dentry: {t:?}"
    );
}

/// Rename a base file then delete it at the new location.
#[test]
fn rename_then_delete() {
    let s = AgfsSession::new().expect("session setup");
    fs::rename(s.mnt_path("hello.txt"), s.mnt_path("temp.txt")).expect("rename");
    fs::remove_file(s.mnt_path("temp.txt")).expect("delete");
    assert_consistent(&s);

    // Both old and new paths should be invisible
    assert!(!path_visible(&s.mnt_path("hello.txt")));
    assert!(!path_visible(&s.mnt_path("temp.txt")));
}

/// Swap two base files via a temporary name.
#[test]
fn swap_via_tmp() {
    let s = AgfsSession::new().expect("session setup");
    fs::rename(s.mnt_path("hello.txt"), s.mnt_path("tmp.txt")).expect("step 1");
    fs::rename(s.mnt_path("multi.txt"), s.mnt_path("hello.txt")).expect("step 2");
    fs::rename(s.mnt_path("tmp.txt"), s.mnt_path("multi.txt")).expect("step 3");
    assert_consistent(&s);

    // Content should be swapped
    assert_eq!(
        fs::read_to_string(s.mnt_path("hello.txt")).unwrap(),
        "line1\nline2\n"
    );
    assert_eq!(
        fs::read_to_string(s.mnt_path("multi.txt")).unwrap(),
        "base content\n"
    );
}

// ── Checkpoint / Restore ─────────────────────────────────────────────

/// Checkpoint, add more changes, restore — state should match checkpoint.
#[test]
fn restore_state() {
    let s = AgfsSession::new().expect("session setup");

    fs::write(s.mnt_path("a.txt"), "aa\n").expect("create a");
    fs::write(s.mnt_path("hello.txt"), "mod\n").expect("modify");
    s.cli(&["checkpoint", "snap"]).expect("checkpoint");

    // Dead zone: changes after checkpoint
    fs::write(s.mnt_path("dead.txt"), "gone\n").expect("create dead");
    fs::remove_file(s.mnt_path("a.txt")).expect("delete a");

    s.cli(&["restore", "snap"]).expect("restore");

    assert_overlay_visible(&s);
    assert_dir_matches(&s, "");
    assert!(path_visible(&s.mnt_path("a.txt")));
    assert!(!path_visible(&s.mnt_path("dead.txt")));
}

/// Two checkpoints, restore to the first one.
#[test]
fn restore_to_earlier_checkpoint() {
    let s = AgfsSession::new().expect("session setup");

    fs::write(s.mnt_path("a.txt"), "v1\n").expect("create");
    s.cli(&["checkpoint", "c1"]).expect("checkpoint 1");

    fs::write(s.mnt_path("b.txt"), "v2\n").expect("create");
    s.cli(&["checkpoint", "c2"]).expect("checkpoint 2");

    s.cli(&["restore", "c1"]).expect("restore to c1");
    assert_overlay_visible(&s);
    assert_dir_matches(&s, "");

    assert!(path_visible(&s.mnt_path("a.txt")));
    assert!(!path_visible(&s.mnt_path("b.txt")));
}

/// Restore with renames: verify Link entries survive the round-trip.
#[test]
fn restore_with_renames() {
    let s = AgfsSession::new().expect("session setup");

    fs::rename(s.mnt_path("hello.txt"), s.mnt_path("moved.txt")).expect("rename");
    fs::write(s.mnt_path("new.txt"), "new\n").expect("create");
    s.cli(&["checkpoint", "snap"]).expect("checkpoint");

    // Dead zone
    fs::remove_file(s.mnt_path("moved.txt")).expect("delete");

    s.cli(&["restore", "snap"]).expect("restore");
    assert_overlay_visible(&s);
    assert_dir_matches(&s, "");

    assert!(path_visible(&s.mnt_path("moved.txt")));
    assert!(!path_visible(&s.mnt_path("hello.txt")));
    assert_eq!(
        fs::read_to_string(s.mnt_path("moved.txt")).unwrap(),
        "base content\n"
    );
}

// ── Create over tombstone ────────────────────────────────────────────

/// Delete a base file then create a new file at the same path.
/// Kernel always uses A tag; the add/modify distinction is derived by
/// userspace checking the base filesystem.
#[test]
fn create_over_tombstone() {
    let s = AgfsSession::new().expect("session setup");
    fs::remove_file(s.mnt_path("hello.txt")).expect("delete base");
    fs::write(s.mnt_path("hello.txt"), "reborn\n").expect("recreate");
    assert_consistent(&s);

    let t = tree(&s);
    assert!(
        t.any(|p, e| p.ends_with("/hello.txt")
            && matches!(e, Target::Inode(_))),
        "recreated file over tombstone should have Target::Inode: {t:?}"
    );
    assert_eq!(
        fs::read_to_string(s.mnt_path("hello.txt")).unwrap(),
        "reborn\n"
    );
}

/// Delete a base dir, recreate it, add files inside.
#[test]
fn recreate_base_dir_with_files() {
    let s = AgfsSession::new().expect("session setup");
    fs::remove_file(s.mnt_path("subdir/deep.txt")).expect("unlink child");
    fs::remove_dir(s.mnt_path("subdir")).expect("rmdir");
    fs::create_dir(s.mnt_path("subdir")).expect("mkdir again");
    fs::write(s.mnt_path("subdir/new.txt"), "fresh\n").expect("create");
    assert_overlay_visible(&s);
    assert_dir_matches(&s, "");
    assert_dir_matches(&s, "subdir");
}

// ── d_type and inode number in readdir ───────────────────────────────

/// Verify that kernel readdir d_type matches CLI dtype for staged entries.
#[test]
fn readdir_dtype_matches_cli() {
    let s = AgfsSession::new().expect("session setup");

    fs::write(s.mnt_path("reg.txt"), "data\n").expect("create file");
    fs::create_dir(s.mnt_path("dir")).expect("mkdir");
    std::os::unix::fs::symlink("reg.txt", s.mnt_path("lnk")).expect("symlink");

    let prefix = root_prefix(&s);
    let t = tree(&s);

    for entry in fs::read_dir(s.mnt_path("")).expect("readdir") {
        let entry = entry.expect("entry");
        let name = entry.file_name().to_string_lossy().to_string();

        // Find matching CLI entry via DirNode to determine expected type
        let tree_path = format!("{prefix}/{name}");
        let Some(node) = t.get_node(&tree_path) else {
            continue; // base-only entry, no CLI overlay
        };

        let ft = entry.file_type().expect("file_type");
        match node {
            DirNode::Dir(_, _) => assert!(
                ft.is_dir(),
                "readdir d_type for '{name}': CLI=Dir but kernel reports file={} sym={}",
                ft.is_file(),
                ft.is_symlink()
            ),
            DirNode::File(_) => assert!(
                ft.is_file() || ft.is_symlink(),
                "readdir d_type for '{name}': CLI=File but kernel reports dir={}",
                ft.is_dir()
            ),
        }
    }
}

/// Verify that kernel readdir ino for staged entries matches stat ino.
#[test]
fn readdir_ino_matches_cli() {
    let s = AgfsSession::new().expect("session setup");

    fs::write(s.mnt_path("check_ino.txt"), "data\n").expect("create");

    let stat_ino = fs::metadata(s.mnt_path("check_ino.txt"))
        .expect("stat")
        .ino();

    for entry in fs::read_dir(s.mnt_path("")).expect("readdir") {
        let entry = entry.expect("entry");
        if entry.file_name() == "check_ino.txt" {
            let kernel_ino = entry.ino();
            assert_eq!(
                kernel_ino, stat_ino,
                "readdir ino mismatch: readdir={kernel_ino} stat={stat_ino}"
            );
            return;
        }
    }
    panic!("check_ino.txt not found in readdir");
}

// ── Deep nesting ─────────────────────────────────────────────────────

/// Operations at 4 levels of directory nesting.
#[test]
fn deep_nesting() {
    let s = AgfsSession::new().expect("session setup");

    fs::create_dir(s.mnt_path("a")).expect("mkdir a");
    fs::create_dir(s.mnt_path("a/b")).expect("mkdir a/b");
    fs::create_dir(s.mnt_path("a/b/c")).expect("mkdir a/b/c");
    fs::write(s.mnt_path("a/b/c/deep.txt"), "deep\n").expect("write");
    fs::write(s.mnt_path("a/top.txt"), "top\n").expect("write");
    fs::write(s.mnt_path("a/b/mid.txt"), "mid\n").expect("write");

    assert_overlay_visible(&s);
    assert_dir_matches(&s, "");
    assert_dir_matches(&s, "a");
    assert_dir_matches(&s, "a/b");
    assert_dir_matches(&s, "a/b/c");
}

/// Rename a file from deep nesting to root, then delete at root.
#[test]
fn deep_rename_to_root() {
    let s = AgfsSession::new().expect("session setup");

    fs::create_dir(s.mnt_path("a")).expect("mkdir a");
    fs::create_dir(s.mnt_path("a/b")).expect("mkdir a/b");
    fs::write(s.mnt_path("a/b/f.txt"), "deep\n").expect("write");
    fs::rename(s.mnt_path("a/b/f.txt"), s.mnt_path("surfaced.txt")).expect("rename");

    assert_overlay_visible(&s);
    assert_dir_matches(&s, "");
    assert_dir_matches(&s, "a");
    assert_dir_matches(&s, "a/b");
    assert_eq!(
        fs::read_to_string(s.mnt_path("surfaced.txt")).unwrap(),
        "deep\n"
    );
}

use super::helpers::{actions, journal};
use crate::helpers::AgfsSession;
use agfs::journal::Action;
use std::fs;
use std::os::unix::fs::DirEntryExt;

// ── Cancelled-entry removal ─────────────────────────────────────────────────
//
// When a staged-only entry (in_base=false) is deleted, the kernel removes
// the dirent entirely rather than keeping a tombstone. These tests verify
// the behavioral consequences of that optimisation.

/// Create a staged-only file, delete it, then recreate it.
/// Both creates should produce Add records (not Modify), because the file
/// never existed in the base layer.
#[test]
fn staged_only_delete_recreate_emits_add() {
    let s = AgfsSession::new().expect("session setup");
    s.run_in_namespace(|| {
        fs::write(s.mnt_path("ephemeral.txt"), "v1\n").expect("create");
        fs::remove_file(s.mnt_path("ephemeral.txt")).expect("delete");
        fs::write(s.mnt_path("ephemeral.txt"), "v2\n").expect("recreate");

        let j = journal(&s);
        let acts = actions(&j);

        let adds: Vec<_> = acts
            .iter()
            .filter(|a| matches!(a, Action::Add { path, .. } if path.ends_with("/ephemeral.txt")))
            .collect();
        assert_eq!(
            adds.len(),
            2,
            "both creates of a staged-only file should produce Add records: {acts:?}"
        );

        let modifies: Vec<_> = acts
            .iter()
            .filter(
                |a| matches!(a, Action::Modify { path, .. } if path.ends_with("/ephemeral.txt")),
            )
            .collect();
        assert!(
            modifies.is_empty(),
            "staged-only file should never produce Modify records: {acts:?}"
        );
    });
}

/// Create a staged-only file, rename it, then recreate at the original name.
/// The recreate should produce Add (not Modify) because the original name
/// was never in base.
#[test]
fn staged_only_rename_recreate_emits_add() {
    let s = AgfsSession::new().expect("session setup");
    s.run_in_namespace(|| {
        fs::write(s.mnt_path("orig.txt"), "v1\n").expect("create");
        fs::rename(s.mnt_path("orig.txt"), s.mnt_path("moved.txt")).expect("rename");
        fs::write(s.mnt_path("orig.txt"), "v2\n").expect("recreate at original");

        let j = journal(&s);
        let acts = actions(&j);

        let adds: Vec<_> = acts
            .iter()
            .filter(|a| matches!(a, Action::Add { path, .. } if path.ends_with("/orig.txt")))
            .collect();
        assert_eq!(
            adds.len(),
            2,
            "both creates at staged-only name should produce Add records: {acts:?}"
        );
    });
}

/// Deleting a staged-only file hides it from readdir.
#[test]
fn staged_only_delete_hides_from_readdir() {
    let s = AgfsSession::new().expect("session setup");
    s.run_in_namespace(|| {
        fs::write(s.mnt_path("vanish.txt"), "gone\n").expect("create");
        fs::remove_file(s.mnt_path("vanish.txt")).expect("delete");

        let entries: Vec<String> = fs::read_dir(s.mnt_path(""))
            .expect("readdir")
            .filter_map(|e| e.ok().map(|e| e.file_name().to_string_lossy().to_string()))
            .collect();

        assert!(
            !entries.contains(&"vanish.txt".to_string()),
            "deleted staged-only file should not appear in readdir: {entries:?}"
        );
    });
}

/// Deleting a staged-only file makes stat return ENOENT.
#[test]
fn staged_only_delete_returns_enoent() {
    let s = AgfsSession::new().expect("session setup");
    s.run_in_namespace(|| {
        fs::write(s.mnt_path("ghost.txt"), "boo\n").expect("create");
        fs::remove_file(s.mnt_path("ghost.txt")).expect("delete");

        let err = fs::metadata(s.mnt_path("ghost.txt")).unwrap_err();
        assert_eq!(
            err.kind(),
            std::io::ErrorKind::NotFound,
            "stat on deleted staged-only file should return ENOENT"
        );
    });
}

// ── Contrast: base file delete + recreate produces Modify ───────────────────

/// Deleting a base file and recreating it should produce Modify (not Add)
/// because the file existed in the base layer.
#[test]
fn base_delete_recreate_emits_modify() {
    let s = AgfsSession::new().expect("session setup");
    s.run_in_namespace(|| {
        // hello.txt is a base seed file
        fs::remove_file(s.mnt_path("hello.txt")).expect("delete base file");
        fs::write(s.mnt_path("hello.txt"), "reborn\n").expect("recreate");

        let j = journal(&s);
        let acts = actions(&j);

        let modifies: Vec<_> = acts
            .iter()
            .filter(|a| matches!(a, Action::Modify { path, .. } if path.ends_with("/hello.txt")))
            .collect();
        assert_eq!(
            modifies.len(),
            1,
            "recreate of base file should produce Modify record: {acts:?}"
        );

        let adds: Vec<_> = acts
            .iter()
            .filter(|a| matches!(a, Action::Add { path, .. } if path.ends_with("/hello.txt")))
            .collect();
        assert!(
            adds.is_empty(),
            "recreate of base file should not produce Add record: {acts:?}"
        );
    });
}

/// Readdir should not leak cancelled entries as visible items.
/// Create multiple staged-only files, delete some, verify exact readdir.
#[test]
fn cancel_no_readdir_leak_multiple() {
    let s = AgfsSession::new().expect("session setup");
    s.run_in_namespace(|| {
        fs::create_dir(s.mnt_path("canceldir")).expect("mkdir");
        fs::write(s.mnt_path("canceldir/keep.txt"), "keep\n").expect("create");
        fs::write(s.mnt_path("canceldir/drop1.txt"), "drop\n").expect("create");
        fs::write(s.mnt_path("canceldir/drop2.txt"), "drop\n").expect("create");
        fs::write(s.mnt_path("canceldir/also_keep.txt"), "keep\n").expect("create");
        fs::remove_file(s.mnt_path("canceldir/drop1.txt")).expect("delete");
        fs::remove_file(s.mnt_path("canceldir/drop2.txt")).expect("delete");

        let entries: Vec<String> = fs::read_dir(s.mnt_path("canceldir"))
            .expect("readdir")
            .filter_map(|e| e.ok().map(|e| e.file_name().to_string_lossy().to_string()))
            .collect();

        assert_eq!(entries.len(), 2, "expected 2 entries, got: {entries:?}");
        assert!(entries.contains(&"keep.txt".to_string()));
        assert!(entries.contains(&"also_keep.txt".to_string()));
    });
}

/// Cancelled entries should not produce stale inode numbers in readdir.
/// After create + delete + recreate, the new entry should have a valid
/// non-zero ino.
#[test]
fn cancel_recreate_has_valid_ino() {
    let s = AgfsSession::new().expect("session setup");
    s.run_in_namespace(|| {
        fs::write(s.mnt_path("ino_check.txt"), "v1\n").expect("create");
        fs::remove_file(s.mnt_path("ino_check.txt")).expect("delete");
        fs::write(s.mnt_path("ino_check.txt"), "v2\n").expect("recreate");

        for entry in fs::read_dir(s.mnt_path("")).expect("readdir") {
            let entry = entry.expect("entry");
            if entry.file_name() == "ino_check.txt" {
                assert_ne!(
                    entry.ino(),
                    0,
                    "recreated file after cancel should have non-zero ino"
                );
                return;
            }
        }
        panic!("ino_check.txt not found in readdir");
    });
}

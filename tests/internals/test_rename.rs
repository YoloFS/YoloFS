use super::helpers::{actions, dirents, ino_for, inode_path, inos, journal};
use crate::helpers::AgfsSession;
use agfs::journal::Action;
use std::fs;

// ── Journal ──────────────────────────────────────────────────────────────────

/// Renaming a file produces a Redirect record with dtype=File.
#[test]
fn rename_produces_rename_record() {
    let s = AgfsSession::new().expect("session setup");
    s.run_in_namespace(|| {
        fs::rename(s.mnt_path("hello.txt"), s.mnt_path("moved.txt")).expect("rename");

        let j = journal(&s);
        let acts = actions(&j);
        assert!(
            acts.iter().any(
                |a| matches!(a, Action::Rename { src, dst, dtype: Some(agfs::journal::DType::File), .. }
                if dst.ends_with("/moved.txt") && src.ends_with("/hello.txt"))
            ),
            "journal should have a Redirect(dtype=File) record for hello.txt → moved.txt: {acts:?}"
        );
    });
}

// ── Inode Store ──────────────────────────────────────────────────────────────────

/// Pure rename of a base file creates no new inode (only journal RDR record).
#[test]
fn pure_rename_creates_no_inode() {
    let s = AgfsSession::new().expect("session setup");
    s.run_in_namespace(|| {
        let inos_before = inos(&s);
        fs::rename(s.mnt_path("hello.txt"), s.mnt_path("moved.txt")).expect("rename");
        let inos_after = inos(&s);

        assert_eq!(
            inos_before, inos_after,
            "pure rename should not create new staged inodes"
        );
    });
}

/// Rename + modify (write after rename) produces an inode at the new path.
#[test]
fn rename_then_write_produces_inode() {
    let s = AgfsSession::new().expect("session setup");
    s.run_in_namespace(|| {
        fs::rename(s.mnt_path("hello.txt"), s.mnt_path("moved.txt")).expect("rename");
        fs::write(s.mnt_path("moved.txt"), "new content\n").expect("write renamed file");

        let ch = dirents(&s);
        let ino = ino_for(&ch, "/moved.txt");

        assert_eq!(
            fs::read_to_string(inode_path(&s, ino)).unwrap(),
            "new content\n"
        );
    });
}

/// Write then rename: inode content still correct under the new path.
#[test]
fn write_then_rename_inode_content() {
    let s = AgfsSession::new().expect("session setup");
    s.run_in_namespace(|| {
        fs::write(s.mnt_path("hello.txt"), "written first\n").expect("write");
        fs::rename(s.mnt_path("hello.txt"), s.mnt_path("final.txt")).expect("rename");

        // Read through mount to verify
        let content = fs::read_to_string(s.mnt_path("final.txt")).unwrap();
        assert_eq!(content, "written first\n");

        // The inode should have the written content — resolves as Renamed + Modified.
        let ch = dirents(&s);
        let ino = ino_for(&ch, "/final.txt");
        assert_eq!(
            fs::read_to_string(inode_path(&s, ino)).unwrap(),
            "written first\n"
        );
    });
}

/// Rename chain: a→b→c. Journal should show two Redirect records.
/// Each carries the dentry path as old (no chain resolution in kernel).
#[test]
fn rename_chain_journal_records() {
    let s = AgfsSession::new().expect("session setup");
    s.run_in_namespace(|| {
        fs::rename(s.mnt_path("hello.txt"), s.mnt_path("step1.txt")).expect("a→b");
        fs::rename(s.mnt_path("step1.txt"), s.mnt_path("step2.txt")).expect("b→c");

        let j = journal(&s);
        let acts = actions(&j);

        // Both renames should produce Redirect records (base-file renames)
        let redirects: Vec<_> = acts
            .iter()
            .filter(|a| matches!(a, Action::Rename { .. }))
            .collect();
        assert_eq!(
            redirects.len(),
            2,
            "chain should produce 2 Redirect records: {acts:?}"
        );
    });
}

/// Rename onto an existing base file (overwrite): the old target
/// is implicitly replaced. No explicit delete record for the target
/// — the rename dirent overwrites whatever was there.
#[test]
fn rename_overwrite_journal() {
    let s = AgfsSession::new().expect("session setup");
    s.run_in_namespace(|| {
        // hello.txt overwrites subdir/deep.txt
        fs::rename(s.mnt_path("hello.txt"), s.mnt_path("subdir/deep.txt")).expect("overwrite");

        let j = journal(&s);
        let acts = actions(&j);

        // Should have Replace(hello.txt → subdir/deep.txt) as a single record
        // (no separate Delete for hello.txt — fused into the RDR/REP record).
        let has_replace = acts.iter().any(|a| {
            matches!(a, Action::Replace { src, dst, .. }
            if dst.ends_with("/deep.txt") && src.ends_with("/hello.txt"))
        });
        assert!(
            has_replace,
            "should have Replace for hello.txt → deep.txt: {acts:?}"
        );
    });
}

/// Rename back and forth: a→b→a. After resolution, no staged dirents
/// should remain (the rename cancels out).
#[test]
fn rename_back_and_forth_no_changes() {
    let s = AgfsSession::new().expect("session setup");
    s.run_in_namespace(|| {
        fs::rename(s.mnt_path("hello.txt"), s.mnt_path("temp.txt")).expect("a→b");
        fs::rename(s.mnt_path("temp.txt"), s.mnt_path("hello.txt")).expect("b→a");

        let ch = dirents(&s);
        assert!(
            ch.is_empty(),
            "rename back and forth should produce no dirents, got: {ch:?}"
        );
    });
}

/// Rename a staged (newly created) file to overwrite a base file.
/// The destination exists in base, so the kernel emits P (Replace) record.
#[test]
fn rename_staged_file_to_base_path() {
    let s = AgfsSession::new().expect("session setup");
    s.run_in_namespace(|| {
        // Create a new staged file
        fs::write(s.mnt_path("brand_new.txt"), "staged content\n").expect("create");
        // Rename it to overwrite multi.txt (which exists in base)
        fs::rename(s.mnt_path("brand_new.txt"), s.mnt_path("multi.txt")).expect("rename");

        let j = journal(&s);
        let acts = actions(&j);

        // All renames now emit a single R or P record.
        // Destination exists in base → P (Replace).
        let has_replace = acts.iter().any(|a| {
            matches!(a, Action::Replace { dst, src, .. }
                if dst.ends_with("/multi.txt") && src.ends_with("/brand_new.txt"))
        });
        assert!(
            has_replace,
            "should have Replace for brand_new.txt → multi.txt: {acts:?}"
        );
        // Verify the file content is correct
        let content = fs::read_to_string(s.mnt_path("multi.txt")).expect("read");
        assert_eq!(content, "staged content\n");
    });
}

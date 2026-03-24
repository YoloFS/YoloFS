use crate::helpers::AgfsSession;
use std::fs;

/// Rename a base file and read it through the new name.
#[test]
fn rename_then_read() {
    let s = AgfsSession::new().expect("session setup");

    fs::rename(s.mnt_path("hello.txt"), s.mnt_path("renamed.txt")).expect("rename");

    // New name is readable with original content
    let content = fs::read_to_string(s.mnt_path("renamed.txt")).expect("read renamed");
    assert_eq!(content, "base content\n");

    // Old name is gone
    assert!(
        fs::read_to_string(s.mnt_path("hello.txt")).is_err(),
        "old name should not exist"
    );
}

/// Rename a base file, then write through the new name (triggers COW).
/// This verifies that COW uses the open file handle (pointing at the
/// old base path) rather than resolving by relpath (which would fail).
#[test]
fn rename_then_write_cow() {
    let s = AgfsSession::new().expect("session setup");

    fs::rename(s.mnt_path("hello.txt"), s.mnt_path("moved.txt")).expect("rename");

    // Write through the new name — triggers COW from old base location
    fs::write(s.mnt_path("moved.txt"), "modified after rename\n").expect("write after rename");

    // Read back through mount
    let content = fs::read_to_string(s.mnt_path("moved.txt")).expect("read moved");
    assert_eq!(content, "modified after rename\n");

    // Base file at original path is unchanged
    let base = fs::read_to_string(s.base_path("hello.txt")).expect("read base");
    assert_eq!(base, "base content\n");
}

/// Rename + write + commit: full lifecycle.
#[test]
fn rename_write_commit() {
    let s = AgfsSession::new().expect("session setup");

    fs::rename(s.mnt_path("hello.txt"), s.mnt_path("final.txt")).expect("rename");
    fs::write(s.mnt_path("final.txt"), "committed content\n").expect("write");

    let output = s.cli(&["commit"]).unwrap();
    assert!(output.contains("Committed"), "commit output: {output}");

    // After commit: new name has new content, old name is gone
    assert_eq!(
        fs::read_to_string(s.base_path("final.txt")).unwrap(),
        "committed content\n"
    );
    assert!(
        !s.base_path("hello.txt").exists(),
        "old path should be deleted after commit"
    );
}

/// Rename a file within a subdirectory.
#[test]
fn rename_nested_file() {
    let s = AgfsSession::new().expect("session setup");

    fs::rename(
        s.mnt_path("subdir/deep.txt"),
        s.mnt_path("subdir/shallow.txt"),
    )
    .expect("rename nested");

    let content = fs::read_to_string(s.mnt_path("subdir/shallow.txt")).expect("read");
    assert_eq!(content, "nested\n");

    assert!(
        fs::read_to_string(s.mnt_path("subdir/deep.txt")).is_err(),
        "old nested path should not exist"
    );
}

// ── Pure rename commit + abort (inode.c: agfs_rename, staging.c: journal) ──

/// Pure rename (no modify) + commit: old path gone, new path has content.
#[test]
fn rename_pure_commit() {
    let s = AgfsSession::new().expect("session setup");

    fs::rename(s.mnt_path("hello.txt"), s.mnt_path("moved.txt")).expect("rename");
    s.cli(&["commit"]).expect("commit");

    assert_eq!(
        fs::read_to_string(s.base_path("moved.txt")).unwrap(),
        "base content\n",
        "renamed file should be at new path in base"
    );
    assert!(
        !s.base_path("hello.txt").exists(),
        "old path should be gone from base after commit"
    );
}

/// Abort after rename: base is untouched.
#[test]
fn rename_abort_preserves_base() {
    let s = AgfsSession::new().expect("session setup");

    fs::rename(s.mnt_path("hello.txt"), s.mnt_path("moved.txt")).expect("rename");
    s.cli(&["abort"]).expect("abort");

    assert_eq!(
        fs::read_to_string(s.base_path("hello.txt")).unwrap(),
        "base content\n",
        "base should be intact after abort"
    );
    assert!(
        !s.base_path("moved.txt").exists(),
        "renamed path should not exist in base after abort"
    );
}

/// Rename a file then create a new file with the old name.
/// The new file should be accessible (not blocked by the rename record).
#[test]
fn rename_then_recreate_old_name() {
    let s = AgfsSession::new().expect("session setup");

    fs::rename(s.mnt_path("hello.txt"), s.mnt_path("moved.txt")).expect("rename");

    // Old name is gone
    assert!(fs::read_to_string(s.mnt_path("hello.txt")).is_err());

    // Create a new file with the old name
    fs::write(s.mnt_path("hello.txt"), "new file\n").expect("recreate");

    // New file should be readable
    let content =
        fs::read_to_string(s.mnt_path("hello.txt")).expect("new hello.txt should be readable");
    assert_eq!(content, "new file\n");

    // Renamed file still accessible at new path
    let moved = fs::read_to_string(s.mnt_path("moved.txt")).expect("moved.txt should be readable");
    assert_eq!(moved, "base content\n");
}

/// Rename + recreate old name + commit: both files should be in base.
#[test]
fn rename_recreate_commit() {
    let s = AgfsSession::new().expect("session setup");

    fs::rename(s.mnt_path("hello.txt"), s.mnt_path("moved.txt")).expect("rename");
    fs::write(s.mnt_path("hello.txt"), "replacement\n").expect("recreate");

    s.cli(&["commit"]).expect("commit");

    // New name has the original content
    assert_eq!(
        fs::read_to_string(s.base_path("moved.txt")).unwrap(),
        "base content\n",
        "renamed file should be at new path in base"
    );

    // Old name now has the replacement content
    assert_eq!(
        fs::read_to_string(s.base_path("hello.txt")).unwrap(),
        "replacement\n",
        "recreated file should be committed to base"
    );
}

/// Deleting a file then renaming it should fail.
#[test]
fn rename_deleted_file_fails() {
    let s = AgfsSession::new().expect("session setup");

    fs::remove_file(s.mnt_path("hello.txt")).expect("unlink");

    let result = fs::rename(s.mnt_path("hello.txt"), s.mnt_path("gone.txt"));
    assert!(
        result.is_err(),
        "renaming a deleted file should fail, got Ok"
    );
}

/// Renaming a nonexistent file should fail.
#[test]
fn rename_nonexistent_fails() {
    let s = AgfsSession::new().expect("session setup");

    let result = fs::rename(s.mnt_path("no_such_file.txt"), s.mnt_path("dest.txt"));
    assert!(result.is_err(), "renaming nonexistent file should fail");
}

/// Rename a→b then b→a: file ends up back at original path.
#[test]
fn rename_back_and_forth() {
    let s = AgfsSession::new().expect("session setup");

    fs::rename(s.mnt_path("hello.txt"), s.mnt_path("temp.txt")).expect("rename a→b");
    fs::rename(s.mnt_path("temp.txt"), s.mnt_path("hello.txt")).expect("rename b→a");

    // File should be back at original path with original content
    let content = fs::read_to_string(s.mnt_path("hello.txt")).expect("read");
    assert_eq!(content, "base content\n");

    // Temp name should not exist
    assert!(
        fs::read_to_string(s.mnt_path("temp.txt")).is_err(),
        "temp name should not exist"
    );
}

/// Rename back and forth then commit: base should be unchanged.
#[test]
fn rename_back_and_forth_commit() {
    let s = AgfsSession::new().expect("session setup");

    fs::rename(s.mnt_path("hello.txt"), s.mnt_path("temp.txt")).expect("rename a→b");
    fs::rename(s.mnt_path("temp.txt"), s.mnt_path("hello.txt")).expect("rename b→a");

    s.cli(&["commit"]).expect("commit");

    // Base should still have the file at original path
    assert_eq!(
        fs::read_to_string(s.base_path("hello.txt")).unwrap(),
        "base content\n"
    );
    assert!(
        !s.base_path("temp.txt").exists(),
        "temp path should not exist in base"
    );
}

/// Three-step roundtrip: a→b→c→a. File ends up back at original path.
#[test]
fn rename_three_step_roundtrip() {
    let s = AgfsSession::new().expect("session setup");

    fs::rename(s.mnt_path("hello.txt"), s.mnt_path("temp1.txt")).expect("a→b");
    fs::rename(s.mnt_path("temp1.txt"), s.mnt_path("temp2.txt")).expect("b→c");
    fs::rename(s.mnt_path("temp2.txt"), s.mnt_path("hello.txt")).expect("c→a");

    // File should be back at original path with original content
    let content = fs::read_to_string(s.mnt_path("hello.txt")).expect("read");
    assert_eq!(content, "base content\n");

    // Temp names should not exist
    assert!(fs::read_to_string(s.mnt_path("temp1.txt")).is_err());
    assert!(fs::read_to_string(s.mnt_path("temp2.txt")).is_err());
}

/// Rename onto an existing file (overwrite target).
#[test]
fn rename_overwrite_existing() {
    let s = AgfsSession::new().expect("session setup");

    // hello.txt and subdir/deep.txt both exist in base
    fs::rename(s.mnt_path("hello.txt"), s.mnt_path("subdir/deep.txt")).expect("rename overwrite");

    // New location has old content (hello.txt's content)
    let content = fs::read_to_string(s.mnt_path("subdir/deep.txt")).expect("read");
    assert_eq!(content, "base content\n");

    // Old name is gone
    assert!(
        fs::read_to_string(s.mnt_path("hello.txt")).is_err(),
        "old name should not exist"
    );
}

/// Rename overwrite + commit: target gets source content, source is gone.
#[test]
fn rename_overwrite_commit() {
    let s = AgfsSession::new().expect("session setup");

    fs::rename(s.mnt_path("hello.txt"), s.mnt_path("subdir/deep.txt")).expect("rename overwrite");
    s.cli(&["commit"]).expect("commit");

    assert_eq!(
        fs::read_to_string(s.base_path("subdir/deep.txt")).unwrap(),
        "base content\n",
        "target should have source content after commit"
    );
    assert!(
        !s.base_path("hello.txt").exists(),
        "source should be gone from base after commit"
    );
}

/// Rename chain: a→b→c through the mount.
#[test]
fn rename_chain() {
    let s = AgfsSession::new().expect("session setup");

    fs::rename(s.mnt_path("hello.txt"), s.mnt_path("step1.txt")).expect("a→b");
    fs::rename(s.mnt_path("step1.txt"), s.mnt_path("step2.txt")).expect("b→c");

    let content = fs::read_to_string(s.mnt_path("step2.txt")).expect("read");
    assert_eq!(content, "base content\n");
    assert!(fs::read_to_string(s.mnt_path("hello.txt")).is_err());
    assert!(fs::read_to_string(s.mnt_path("step1.txt")).is_err());
}

/// Rename chain + commit: only final name in base.
#[test]
fn rename_chain_commit() {
    let s = AgfsSession::new().expect("session setup");

    fs::rename(s.mnt_path("hello.txt"), s.mnt_path("step1.txt")).expect("a→b");
    fs::rename(s.mnt_path("step1.txt"), s.mnt_path("step2.txt")).expect("b→c");
    s.cli(&["commit"]).expect("commit");

    assert_eq!(
        fs::read_to_string(s.base_path("step2.txt")).unwrap(),
        "base content\n"
    );
    assert!(!s.base_path("hello.txt").exists());
    assert!(!s.base_path("step1.txt").exists());
}

/// Rename two different base files such that one's destination is the other's
/// original path: mv hello.txt→moved.txt, mv multi.txt→hello.txt, then commit.
/// Both files should land at their new paths with their original content.
#[test]
fn rename_swap_like_commit() {
    let s = AgfsSession::new().expect("session setup");

    fs::rename(s.mnt_path("hello.txt"), s.mnt_path("moved.txt")).expect("rename hello→moved");
    fs::rename(s.mnt_path("multi.txt"), s.mnt_path("hello.txt")).expect("rename multi→hello");

    s.cli(&["commit"]).expect("commit");

    assert_eq!(
        fs::read_to_string(s.base_path("moved.txt")).unwrap(),
        "base content\n",
        "moved.txt should have hello.txt's original content"
    );
    assert_eq!(
        fs::read_to_string(s.base_path("hello.txt")).unwrap(),
        "line1\nline2\n",
        "hello.txt should have multi.txt's original content"
    );
    assert!(
        !s.base_path("multi.txt").exists(),
        "multi.txt should be gone from base"
    );
}

/// True cyclic swap: mv hello.txt→tmp, mv multi.txt→hello.txt,
/// mv tmp→multi.txt.  Both files exchange content after commit.
#[test]
fn rename_cyclic_swap_commit() {
    let s = AgfsSession::new().expect("session setup");

    fs::rename(s.mnt_path("hello.txt"), s.mnt_path("tmp")).expect("rename hello→tmp");
    fs::rename(s.mnt_path("multi.txt"), s.mnt_path("hello.txt")).expect("rename multi→hello");
    fs::rename(s.mnt_path("tmp"), s.mnt_path("multi.txt")).expect("rename tmp→multi");

    // Verify through mount before commit.
    assert_eq!(
        fs::read_to_string(s.mnt_path("hello.txt")).unwrap(),
        "line1\nline2\n"
    );
    assert_eq!(
        fs::read_to_string(s.mnt_path("multi.txt")).unwrap(),
        "base content\n"
    );

    s.cli(&["commit"]).expect("commit");

    assert_eq!(
        fs::read_to_string(s.base_path("hello.txt")).unwrap(),
        "line1\nline2\n",
        "hello.txt should now have multi.txt's original content"
    );
    assert_eq!(
        fs::read_to_string(s.base_path("multi.txt")).unwrap(),
        "base content\n",
        "multi.txt should now have hello.txt's original content"
    );
}

/// Rename a directory and verify contents are accessible through new name.
#[test]
fn rename_directory_with_contents() {
    let s = AgfsSession::new().expect("session setup");

    fs::rename(s.mnt_path("subdir"), s.mnt_path("moved_dir")).expect("rename dir");

    // Contents readable through new name
    let content = fs::read_to_string(s.mnt_path("moved_dir/deep.txt")).expect("read nested");
    assert_eq!(content, "nested\n");

    // Old name is gone
    assert!(
        !s.mnt_path("subdir").exists(),
        "old directory name should not exist"
    );
}

/// Rename a directory + commit: directory and contents end up in base.
#[test]
fn rename_directory_commit() {
    let s = AgfsSession::new().expect("session setup");

    fs::rename(s.mnt_path("subdir"), s.mnt_path("moved_dir")).expect("rename dir");
    s.cli(&["commit"]).expect("commit");

    assert_eq!(
        fs::read_to_string(s.base_path("moved_dir/deep.txt")).unwrap(),
        "nested\n"
    );
    assert!(!s.base_path("subdir").exists());
}

/// Rename a symlink and verify the target is preserved.
#[test]
fn rename_symlink_preserves_target() {
    let s = AgfsSession::new().expect("session setup");

    std::os::unix::fs::symlink("hello.txt", s.mnt_path("link")).expect("create symlink");
    fs::rename(s.mnt_path("link"), s.mnt_path("moved_link")).expect("rename symlink");

    let target = std::fs::read_link(s.mnt_path("moved_link")).expect("read link target");
    assert_eq!(target, std::path::Path::new("hello.txt"));

    assert!(
        !s.mnt_path("link").exists(),
        "old symlink name should not exist"
    );
}

/// Create a staged file, rename it to a new path, then commit.
/// Tests the commit guard: base rename is skipped (source doesn't exist in base)
/// and content is applied via the staged inode.
#[test]
fn rename_staged_file_to_new_path_commit() {
    let s = AgfsSession::new().expect("session setup");

    fs::write(s.mnt_path("brand_new.txt"), "staged content\n").expect("create staged");
    fs::rename(s.mnt_path("brand_new.txt"), s.mnt_path("final.txt")).expect("rename staged");

    // Verify through mount
    let content = fs::read_to_string(s.mnt_path("final.txt")).expect("read through mount");
    assert_eq!(content, "staged content\n");
    assert!(
        fs::read_to_string(s.mnt_path("brand_new.txt")).is_err(),
        "old name should not exist"
    );

    s.cli(&["commit"]).expect("commit");

    // After commit: new name in base, old name absent
    assert_eq!(
        fs::read_to_string(s.base_path("final.txt")).unwrap(),
        "staged content\n",
        "staged file should be committed at new path"
    );
    assert!(
        !s.base_path("brand_new.txt").exists(),
        "old staged path should not exist in base"
    );
}

/// Create a staged file and rename it to overwrite a base file, then commit.
/// Tests the DEL + MOD path: the base file is replaced with staged content.
#[test]
fn rename_staged_file_overwrite_base_commit() {
    let s = AgfsSession::new().expect("session setup");

    fs::write(s.mnt_path("brand_new.txt"), "replacement\n").expect("create staged");
    fs::rename(s.mnt_path("brand_new.txt"), s.mnt_path("multi.txt")).expect("rename overwrite");

    // Verify through mount
    let content = fs::read_to_string(s.mnt_path("multi.txt")).expect("read through mount");
    assert_eq!(content, "replacement\n");

    s.cli(&["commit"]).expect("commit");

    // After commit: base file overwritten with staged content
    assert_eq!(
        fs::read_to_string(s.base_path("multi.txt")).unwrap(),
        "replacement\n",
        "base file should be overwritten with staged content"
    );
    assert!(
        !s.base_path("brand_new.txt").exists(),
        "staged-only file should not appear in base"
    );
}

/// Complex multi-operation scenario exercising the ADD/MOD/DEL/RDR journal format,
/// resolver edge cases, and commit correctness in a single session.
///
/// Base files: hello.txt, multi.txt, subdir/deep.txt, test.sh
///
/// Operations (grouped by the edge case they exercise):
///   1. Modify hello.txt (COW → MOD record)
///   2. Create brand_new.txt then rename it to multi.txt
///      (staged rename to base path → DEL + MOD; overwrites base file)
///   3. Create temp.txt then delete it (ADD + DEL cancel out)
///   4. Rename subdir/deep.txt → subdir/shallow.txt (base rename → DEL + RDR)
///   5. Rename subdir/shallow.txt → top.txt (second rename → DEL + RDR)
///   6. Create link.txt as symlink (A with dtype=Link)
///   7. Modify hello.txt again (second COW → M; multiple modifies keep final ino)
///
/// Expected resolved state before commit:
///   - Modified(hello.txt)           — double COW, final ino wins
///   - Modified(multi.txt)           — staged file overwrote base via rename
///   - Renamed(subdir/shallow.txt → top.txt) — second rename
///   - Deleted(subdir/deep.txt)      — first rename source
///   - Added(link.txt)               — new symlink
///   - (temp.txt absent)             — ADD + DEL cancelled
///
/// After commit all dirents are applied to base.
#[test]
fn complex_multi_operation_commit() {
    let s = AgfsSession::new().expect("session setup");

    // ── 1. COW modify hello.txt ──
    fs::write(s.mnt_path("hello.txt"), "first edit\n").expect("write hello v1");

    // ── 2. Create staged file, rename onto base file (DEL + MOD path) ──
    fs::write(s.mnt_path("brand_new.txt"), "replacement\n").expect("create brand_new");
    fs::rename(s.mnt_path("brand_new.txt"), s.mnt_path("multi.txt"))
        .expect("rename brand_new → multi");

    // ── 3. Create then immediately delete (ADD + DEL cancel) ──
    fs::write(s.mnt_path("temp.txt"), "ephemeral\n").expect("create temp");
    fs::remove_file(s.mnt_path("temp.txt")).expect("delete temp");

    // ── 4 + 5. Chained rename: subdir/deep.txt → subdir/shallow.txt → top.txt ──
    fs::rename(
        s.mnt_path("subdir/deep.txt"),
        s.mnt_path("subdir/shallow.txt"),
    )
    .expect("rename deep → shallow");
    fs::rename(s.mnt_path("subdir/shallow.txt"), s.mnt_path("top.txt"))
        .expect("rename shallow → top");

    // ── 6. Create a symlink (ADD record, dtype=Link) ──
    std::os::unix::fs::symlink("hello.txt", s.mnt_path("link.txt")).expect("symlink");

    // ── 7. Second COW on hello.txt (multiple MOD records → final ino wins) ──
    fs::write(s.mnt_path("hello.txt"), "second edit\n").expect("write hello v2");

    // ── Verify mount view before commit ──
    assert_eq!(
        fs::read_to_string(s.mnt_path("hello.txt")).unwrap(),
        "second edit\n"
    );
    assert_eq!(
        fs::read_to_string(s.mnt_path("multi.txt")).unwrap(),
        "replacement\n"
    );
    assert!(!s.mnt_path("temp.txt").exists(), "temp.txt should be gone");
    assert!(
        !s.mnt_path("brand_new.txt").exists(),
        "brand_new.txt should be gone (renamed)"
    );
    assert!(
        !s.mnt_path("subdir/deep.txt").exists(),
        "deep.txt should be gone (renamed)"
    );
    assert!(
        !s.mnt_path("subdir/shallow.txt").exists(),
        "shallow.txt should be gone (renamed again)"
    );
    assert_eq!(
        fs::read_to_string(s.mnt_path("top.txt")).unwrap(),
        "nested\n",
        "top.txt should have deep.txt content"
    );
    assert_eq!(
        fs::read_link(s.mnt_path("link.txt")).unwrap(),
        std::path::Path::new("hello.txt")
    );

    // ── Verify resolved dirents ──
    use agfs::journal;
    use agfs::journal::{Dentry, Target};

    let agfs_dir = s.root.join(".agfs");
    let journal_obj = journal::Journal::read(&agfs_dir).expect("read journal");
    let t = journal_obj.into_tree();

    let (
        mut has_modified_hello,
        mut has_modified_multi,
        mut has_renamed_deep_to_top,
        mut has_deleted_deep,
        mut has_added_link,
        mut has_temp,
        mut has_brand_new,
    ) = (false, false, false, false, false, false, false);

    t.for_each(|path, dentry| {
        if matches!(dentry, Dentry { target: Target::Inode(_), in_base: true, .. })
            && path.ends_with("/hello.txt")
        {
            has_modified_hello = true;
        }
        if matches!(dentry, Dentry { target: Target::Inode(_), in_base: true, .. })
            && path.ends_with("/multi.txt")
        {
            has_modified_multi = true;
        }
        // ── 4 + 5. Chained rename: subdir/deep.txt → subdir/shallow.txt → top.txt ──
        // Tree builder preserves original base path through rename chains.
        if let Target::Path(Some(src)) = &dentry.target {
            if src.ends_with("/subdir/deep.txt") && path.ends_with("/top.txt") {
                has_renamed_deep_to_top = true;
            }
            if src.ends_with("/temp.txt") {
                has_temp = true;
            }
            if src.ends_with("/brand_new.txt") {
                has_brand_new = true;
            }
        }
        if matches!(dentry.target, Target::None) && path.ends_with("/deep.txt") {
            has_deleted_deep = true;
        }
        if matches!(dentry, Dentry { target: Target::Inode(_), in_base: false, .. })
            && path.ends_with("/link.txt")
        {
            has_added_link = true;
        }
        if path.ends_with("/temp.txt") {
            has_temp = true;
        }
        if path.ends_with("/brand_new.txt") {
            has_brand_new = true;
        }
    });

    assert!(has_modified_hello, "expected Modified(hello.txt): {t:?}");
    assert!(has_modified_multi, "expected Modified(multi.txt): {t:?}");
    assert!(
        has_renamed_deep_to_top,
        "expected Renamed(subdir/deep.txt → top.txt): {t:?}"
    );
    assert!(has_deleted_deep, "expected Deleted(deep.txt): {t:?}");
    assert!(has_added_link, "expected Added(link.txt): {t:?}");
    assert!(!has_temp, "temp.txt should have cancelled out (A+D): {t:?}");
    assert!(
        !has_brand_new,
        "brand_new.txt should not appear (staged rename absorbed): {t:?}"
    );

    // ── Commit and verify base ──
    s.cli(&["commit"]).expect("commit");

    // hello.txt: second edit
    assert_eq!(
        fs::read_to_string(s.base_path("hello.txt")).unwrap(),
        "second edit\n",
        "hello.txt should have final edit"
    );

    // multi.txt: replaced by staged file
    assert_eq!(
        fs::read_to_string(s.base_path("multi.txt")).unwrap(),
        "replacement\n",
        "multi.txt should have staged replacement"
    );

    // subdir/deep.txt → top.txt (renamed via chain)
    assert_eq!(
        fs::read_to_string(s.base_path("top.txt")).unwrap(),
        "nested\n",
        "top.txt should have deep.txt content"
    );
    assert!(
        !s.base_path("subdir/deep.txt").exists(),
        "deep.txt should be gone from base"
    );

    // link.txt: symlink
    assert_eq!(
        fs::read_link(s.base_path("link.txt")).unwrap(),
        std::path::Path::new("hello.txt"),
        "symlink should be committed"
    );

    // Ephemeral files should not exist in base
    assert!(
        !s.base_path("temp.txt").exists(),
        "temp.txt should not be in base"
    );
    assert!(
        !s.base_path("brand_new.txt").exists(),
        "brand_new.txt should not be in base"
    );

    // test.sh: untouched
    assert_eq!(
        fs::read_to_string(s.base_path("test.sh")).unwrap(),
        "#!/bin/sh\necho ok\n",
        "test.sh should be untouched"
    );
}

/// Move a child file out of a directory, then rename the parent directory.
/// Both the extracted file and the renamed directory should appear in base.
#[test]
fn rename_child_then_parent_commit() {
    let s = AgfsSession::new().expect("session setup");

    // Move deep.txt out of subdir, then rename subdir itself.
    fs::rename(s.mnt_path("subdir/deep.txt"), s.mnt_path("extracted.txt"))
        .expect("rename deep → extracted");
    fs::rename(s.mnt_path("subdir"), s.mnt_path("renamed_dir"))
        .expect("rename subdir → renamed_dir");

    // Verify through mount.
    assert_eq!(
        fs::read_to_string(s.mnt_path("extracted.txt")).unwrap(),
        "nested\n"
    );
    assert!(s.mnt_path("renamed_dir").exists());
    assert!(!s.mnt_path("subdir").exists());

    s.cli(&["commit"]).expect("commit");

    assert_eq!(
        fs::read_to_string(s.base_path("extracted.txt")).unwrap(),
        "nested\n",
        "extracted.txt should have deep.txt's content"
    );
    assert!(
        s.base_path("renamed_dir").exists(),
        "renamed_dir should exist in base"
    );
    assert!(
        !s.base_path("subdir").exists(),
        "subdir should be gone from base"
    );
    assert!(
        !s.base_path("subdir/deep.txt").exists(),
        "subdir/deep.txt should be gone from base"
    );
}

/// Same as rename_child_then_parent_commit but with destination names
/// chosen so the parent rename sorts first alphabetically (BTreeMap order).
/// This exposes ordering bugs: the parent directory rename must not run
/// before the child file is extracted.
#[test]
fn rename_child_then_parent_commit_reversed_order() {
    let s = AgfsSession::new().expect("session setup");

    // "a_dir" < "zoo.txt" — forces parent rename first in BTreeMap order.
    fs::rename(s.mnt_path("subdir/deep.txt"), s.mnt_path("zoo.txt")).expect("rename deep → zoo");
    fs::rename(s.mnt_path("subdir"), s.mnt_path("a_dir")).expect("rename subdir → a_dir");

    assert_eq!(
        fs::read_to_string(s.mnt_path("zoo.txt")).unwrap(),
        "nested\n"
    );
    assert!(s.mnt_path("a_dir").exists());

    s.cli(&["commit"]).expect("commit");

    assert_eq!(
        fs::read_to_string(s.base_path("zoo.txt")).unwrap(),
        "nested\n",
        "zoo.txt should have deep.txt's content"
    );
    assert!(s.base_path("a_dir").exists(), "a_dir should exist in base");
    assert!(
        !s.base_path("subdir").exists(),
        "subdir should be gone from base"
    );
}

/// Rename chain a→b→c: the kernel follows the link's base_path so the
/// final link points to the original source, not an intermediate link.
#[test]
fn rename_chain_follows_link_base_path() {
    let s = AgfsSession::new().expect("session setup");

    // Rename a base file twice: hello.txt → step1 → step2
    fs::rename(s.mnt_path("hello.txt"), s.mnt_path("step1.txt")).expect("a→b");
    fs::rename(s.mnt_path("step1.txt"), s.mnt_path("step2.txt")).expect("b→c");

    // The final name should read the original content
    let content = fs::read_to_string(s.mnt_path("step2.txt")).expect("read step2");
    assert_eq!(content, "base content\n");

    // Checkpoint + restore to verify the chain survives serialization
    s.cli(&["checkpoint", "chk1"]).expect("checkpoint");
    fs::write(s.mnt_path("extra.txt"), "extra\n").expect("write post");
    s.cli(&["restore", "chk1"]).expect("restore");

    let content = fs::read_to_string(s.mnt_path("step2.txt")).expect("read after restore");
    assert_eq!(content, "base content\n");
    assert!(
        !s.mnt_path("hello.txt").exists(),
        "original name should be hidden"
    );
    assert!(
        !s.mnt_path("step1.txt").exists(),
        "intermediate name should be hidden"
    );
}

/// Replace /a → /b (overwrite) then delete /b. Both /a and /b had base
/// content, so both must have tombstones. Without the tombstone at /b,
/// the base content would reappear after restore.
#[test]
fn replace_then_delete_tombstones_both() {
    let s = AgfsSession::new().expect("session setup");

    // hello.txt and multi.txt both exist in base
    fs::rename(s.mnt_path("hello.txt"), s.mnt_path("multi.txt")).expect("replace");
    fs::remove_file(s.mnt_path("multi.txt")).expect("delete multi");

    // Both should be gone through the mount
    assert!(
        !s.mnt_path("hello.txt").exists(),
        "hello.txt should be hidden"
    );
    assert!(
        !s.mnt_path("multi.txt").exists(),
        "multi.txt should be hidden"
    );

    // Checkpoint + restore to verify tombstones survive serialization
    s.cli(&["checkpoint", "chk1"]).expect("checkpoint");
    fs::write(s.mnt_path("extra.txt"), "extra\n").expect("write post");
    s.cli(&["restore", "chk1"]).expect("restore");

    assert!(
        !s.mnt_path("hello.txt").exists(),
        "hello.txt should be hidden after restore"
    );
    assert!(
        !s.mnt_path("multi.txt").exists(),
        "multi.txt should be hidden after restore (tombstone must survive)"
    );
}

use crate::helpers::YoloSession;
use std::fs;
use std::os::unix::fs::PermissionsExt;

#[test]
fn commit_modified_file() {
    let s = YoloSession::new().expect("session setup");

    fs::write(s.mnt_path("hello.txt"), "committed\n").unwrap();

    let output = s.cli_stderr(&["commit"]).expect("commit");
    assert!(output.contains("committed 1 change"), "output: {output}");

    // Base file now has the committed content
    assert_eq!(
        fs::read_to_string(s.base_path("hello.txt")).unwrap(),
        "committed\n"
    );
}

#[test]
fn commit_new_file() {
    let s = YoloSession::new().expect("session setup");

    fs::write(s.mnt_path("brandnew.txt"), "new\n").unwrap();

    let output = s.cli_stderr(&["commit"]).expect("commit");
    assert!(output.contains("committed 1 change"), "output: {output}");

    // New file now in base
    assert_eq!(
        fs::read_to_string(s.base_path("brandnew.txt")).unwrap(),
        "new\n"
    );
}

#[test]
fn commit_multiple_changes() {
    let s = YoloSession::new().expect("session setup");

    fs::write(s.mnt_path("hello.txt"), "changed\n").unwrap();
    fs::write(s.mnt_path("newfile.txt"), "new\n").unwrap();

    let output = s.cli_stderr(&["commit"]).expect("commit");
    assert!(output.contains("committed 2 change"), "output: {output}");

    assert_eq!(
        fs::read_to_string(s.base_path("hello.txt")).unwrap(),
        "changed\n"
    );
    assert_eq!(
        fs::read_to_string(s.base_path("newfile.txt")).unwrap(),
        "new\n"
    );
}

#[test]
fn commit_nothing() {
    let s = YoloSession::new().expect("session setup");

    let output = s.cli_stderr(&["commit"]).expect("commit");
    assert!(output.contains("nothing to commit"), "output: {output}");
}

/// Delete a directory, create a file with the same name, commit.
/// The commit should replace the directory with the file in base.
#[test]
fn commit_replace_dir_with_file() {
    let s = YoloSession::new().expect("session setup");

    // subdir/ exists in base as a directory
    assert!(s.base_path("subdir").is_dir());

    // Remove the directory and create a file with the same name
    fs::remove_dir_all(s.mnt_path("subdir")).expect("rmdir");
    fs::write(s.mnt_path("subdir"), "now a file\n").expect("write");

    let content = fs::read_to_string(s.mnt_path("subdir")).expect("read");
    assert_eq!(content, "now a file\n");

    s.cli(&["commit"]).expect("commit");

    // Base should now have a file, not a directory
    assert!(
        !s.base_path("subdir").is_dir(),
        "base subdir should no longer be a directory after commit"
    );
    assert_eq!(
        fs::read_to_string(s.base_path("subdir")).unwrap(),
        "now a file\n",
        "base should have the file content"
    );
}

/// Commit after rename: the renamed file should appear at its new location in base.
#[test]
fn commit_rename_file() {
    let s = YoloSession::new().expect("session setup");

    // Rename an existing base file through the mount
    fs::rename(s.mnt_path("hello.txt"), s.mnt_path("greeting.txt")).expect("rename");

    let output = s.cli_stderr(&["commit"]).expect("commit");
    assert!(output.contains("committed"), "output: {output}");

    assert!(
        !s.base_path("hello.txt").exists(),
        "original should be gone from base"
    );
    assert_eq!(
        fs::read_to_string(s.base_path("greeting.txt")).unwrap(),
        "base content\n",
        "renamed file should have original content in base"
    );
}

/// Commit after renaming a directory: children should follow.
#[test]
fn commit_rename_directory() {
    let s = YoloSession::new().expect("session setup");

    // subdir/ exists in base with subdir/deep.txt
    fs::rename(s.mnt_path("subdir"), s.mnt_path("renamed_dir")).expect("rename dir");

    let output = s.cli_stderr(&["commit"]).expect("commit");
    assert!(output.contains("committed"), "output: {output}");

    assert!(
        !s.base_path("subdir").exists(),
        "original dir should be gone from base"
    );
    assert!(
        s.base_path("renamed_dir").is_dir(),
        "renamed dir should exist in base"
    );
    assert!(
        s.base_path("renamed_dir/deep.txt").exists(),
        "child file should follow the directory rename"
    );
}

/// Commit after deleting a directory: the directory and its children should be removed from base.
#[test]
fn commit_delete_directory() {
    let s = YoloSession::new().expect("session setup");

    // subdir/ exists in base with subdir/deep.txt
    fs::remove_dir_all(s.mnt_path("subdir")).expect("rmdir");

    let output = s.cli_stderr(&["commit"]).expect("commit");
    assert!(output.contains("committed"), "output: {output}");

    assert!(
        !s.base_path("subdir").exists(),
        "deleted dir should be gone from base"
    );
    assert!(
        !s.base_path("subdir/deep.txt").exists(),
        "children should be gone from base"
    );
}

#[test]
fn commit_preserves_readonly_mode() {
    use std::os::unix::fs::MetadataExt;

    let s = YoloSession::new().expect("session setup");

    let base = s.base_path("hello.txt");
    fs::set_permissions(&base, fs::Permissions::from_mode(0o444)).expect("chmod");

    fs::write(s.mnt_path("hello.txt"), "committed\n").expect("write");
    s.cli(&["commit"]).expect("commit");

    let meta = fs::metadata(&base).unwrap();
    assert_eq!(
        meta.mode() & 0o777,
        0o444,
        "committed file should preserve original mode 0444, got {:o}",
        meta.mode() & 0o777
    );
    assert_eq!(fs::read_to_string(&base).unwrap(), "committed\n");
}

/// Committing a modified executable file should preserve its 0o755 mode.
#[test]
fn commit_preserves_executable_mode() {
    use std::os::unix::fs::MetadataExt;

    let s = YoloSession::new().expect("session setup");

    // test.sh is seeded with mode 0o755
    fs::write(s.mnt_path("test.sh"), "#!/bin/sh\necho committed\n").expect("write");
    s.cli(&["commit"]).expect("commit");

    let base = s.base_path("test.sh");
    let meta = fs::metadata(&base).unwrap();
    assert_eq!(
        meta.mode() & 0o777,
        0o755,
        "committed executable should preserve mode 0755, got {:o}",
        meta.mode() & 0o777
    );
}

/// Committing a newly created file should keep its default mode (not
/// inherit from some other file).
#[test]
fn commit_new_file_has_default_mode() {
    use std::os::unix::fs::MetadataExt;

    let s = YoloSession::new().expect("session setup");

    fs::write(s.mnt_path("brand_new.txt"), "new\n").expect("write");
    s.cli(&["commit"]).expect("commit");

    let base = s.base_path("brand_new.txt");
    let meta = fs::metadata(&base).unwrap();
    let mode = meta.mode() & 0o777;
    // New files get the default mode from the creating process's umask
    assert!(
        mode == 0o644 || mode == 0o664,
        "new file should have reasonable default mode, got {:o}",
        mode
    );
}

/// Rename a file onto another existing file (overwrite rename).
/// The destination should be overwritten with the source content.
#[test]
fn commit_rename_overwrite() {
    let s = YoloSession::new().expect("session setup");

    // hello.txt and multi.txt both exist in base
    assert!(s.base_path("hello.txt").exists());
    assert!(s.base_path("multi.txt").exists());

    // Rename hello.txt → multi.txt through the mount (overwrites multi.txt)
    fs::rename(s.mnt_path("hello.txt"), s.mnt_path("multi.txt")).expect("rename");

    s.cli(&["commit"]).expect("commit");

    assert!(
        !s.base_path("hello.txt").exists(),
        "source should be gone from base"
    );
    assert_eq!(
        fs::read_to_string(s.base_path("multi.txt")).unwrap(),
        "base content\n",
        "destination should have source content"
    );
}

/// Rename a directory onto another existing directory.
/// Children from both should be merged.
#[test]
fn commit_rename_dir_onto_dir() {
    let s = YoloSession::new().expect("session setup");

    // Create a second directory with content
    fs::create_dir_all(s.mnt_path("otherdir")).expect("mkdir");
    fs::write(s.mnt_path("otherdir/other.txt"), "other\n").expect("write");
    s.cli(&["commit"]).expect("commit first batch");

    // Now rename subdir → otherdir (overwrite)
    fs::remove_dir_all(s.mnt_path("otherdir")).expect("remove otherdir");
    fs::rename(s.mnt_path("subdir"), s.mnt_path("otherdir")).expect("rename dir");

    s.cli(&["commit"]).expect("commit rename");

    assert!(!s.base_path("subdir").exists(), "source dir should be gone");
    assert!(s.base_path("otherdir").is_dir(), "dest dir should exist");
    assert!(
        s.base_path("otherdir/deep.txt").exists(),
        "child from source dir should be present"
    );
}

/// Create a symlink through the mount, commit, verify it appears in base.
#[test]
fn commit_symlink() {
    let s = YoloSession::new().expect("session setup");

    std::os::unix::fs::symlink("hello.txt", s.mnt_path("link.txt")).expect("symlink");

    let output = s.cli_stderr(&["commit"]).expect("commit");
    assert!(output.contains("committed"), "output: {output}");

    let base_link = s.base_path("link.txt");
    assert!(
        base_link
            .symlink_metadata()
            .unwrap()
            .file_type()
            .is_symlink(),
        "base should have a symlink"
    );
    assert_eq!(
        fs::read_link(&base_link).unwrap().to_string_lossy(),
        "hello.txt",
        "symlink target should match"
    );
}

/// Modify a symlink's target, commit, verify the new target in base.
#[test]
fn commit_modified_symlink() {
    let s = YoloSession::new().expect("session setup");

    // Create and commit an initial symlink
    std::os::unix::fs::symlink("hello.txt", s.mnt_path("link.txt")).expect("symlink");
    s.cli(&["commit"]).expect("commit initial");

    // Replace the symlink with a new target
    fs::remove_file(s.mnt_path("link.txt")).expect("remove link");
    std::os::unix::fs::symlink("multi.txt", s.mnt_path("link.txt")).expect("new symlink");

    s.cli(&["commit"]).expect("commit modified");

    let base_link = s.base_path("link.txt");
    assert!(
        base_link
            .symlink_metadata()
            .unwrap()
            .file_type()
            .is_symlink(),
        "should still be a symlink"
    );
    assert_eq!(
        fs::read_link(&base_link).unwrap().to_string_lossy(),
        "multi.txt",
        "symlink should point to new target"
    );
}

/// chmod a new file through the mount, commit — mode should be preserved in base.
#[test]
fn commit_new_file_with_chmod() {
    use std::os::unix::fs::MetadataExt;

    let s = YoloSession::new().expect("session setup");

    fs::write(s.mnt_path("script.sh"), "#!/bin/sh\necho hi\n").expect("write");
    fs::set_permissions(s.mnt_path("script.sh"), fs::Permissions::from_mode(0o755)).expect("chmod");

    // Verify mode is visible through the mount.
    let mnt_meta = fs::metadata(s.mnt_path("script.sh")).unwrap();
    assert_eq!(
        mnt_meta.mode() & 0o777,
        0o755,
        "mount should show 0755, got {:o}",
        mnt_meta.mode() & 0o777
    );

    s.cli(&["commit"]).expect("commit");

    let base_meta = fs::metadata(s.base_path("script.sh")).unwrap();
    assert_eq!(
        base_meta.mode() & 0o777,
        0o755,
        "committed file should have mode 0755, got {:o}",
        base_meta.mode() & 0o777
    );
}

/// chmod a new file to restrictive mode, commit — mode should be preserved.
#[test]
fn commit_new_file_with_restrictive_chmod() {
    use std::os::unix::fs::MetadataExt;

    let s = YoloSession::new().expect("session setup");

    fs::write(s.mnt_path("secret.txt"), "secret\n").expect("write");
    fs::set_permissions(s.mnt_path("secret.txt"), fs::Permissions::from_mode(0o600))
        .expect("chmod");

    s.cli(&["commit"]).expect("commit");

    let base_meta = fs::metadata(s.base_path("secret.txt")).unwrap();
    assert_eq!(
        base_meta.mode() & 0o777,
        0o600,
        "committed file should have mode 0600, got {:o}",
        base_meta.mode() & 0o777
    );
}

/// chmod an existing base file through the mount (COW), commit — new mode in base.
#[test]
fn commit_chmod_existing_file() {
    use std::os::unix::fs::MetadataExt;

    let s = YoloSession::new().expect("session setup");

    // hello.txt exists in base with default mode.
    // Modify content (triggers COW) and change mode.
    fs::write(s.mnt_path("hello.txt"), "modified\n").expect("write");
    fs::set_permissions(s.mnt_path("hello.txt"), fs::Permissions::from_mode(0o700)).expect("chmod");

    let mnt_meta = fs::metadata(s.mnt_path("hello.txt")).unwrap();
    assert_eq!(
        mnt_meta.mode() & 0o777,
        0o700,
        "mount should show 0700 after chmod"
    );

    s.cli(&["commit"]).expect("commit");

    let base_meta = fs::metadata(s.base_path("hello.txt")).unwrap();
    assert_eq!(
        base_meta.mode() & 0o777,
        0o700,
        "committed file should have new mode 0700, got {:o}",
        base_meta.mode() & 0o777
    );
}

/// Ownership of a new file should be preserved through commit.
#[test]
fn commit_new_file_preserves_ownership() {
    use std::os::unix::fs::MetadataExt;

    let s = YoloSession::new().expect("session setup");
    let uid = nix::unistd::getuid().as_raw();
    let gid = nix::unistd::getgid().as_raw();

    fs::write(s.mnt_path("owned.txt"), "mine\n").expect("write");
    s.cli(&["commit"]).expect("commit");

    let base_meta = fs::metadata(s.base_path("owned.txt")).unwrap();
    assert_eq!(
        base_meta.uid(),
        uid,
        "committed file should be owned by uid {uid}, got {}",
        base_meta.uid()
    );
    assert_eq!(
        base_meta.gid(),
        gid,
        "committed file should have gid {gid}, got {}",
        base_meta.gid()
    );
}

/// Ownership of a COW file should be preserved through commit.
#[test]
fn commit_cow_file_preserves_ownership() {
    use std::os::unix::fs::MetadataExt;

    let s = YoloSession::new().expect("session setup");
    let uid = nix::unistd::getuid().as_raw();
    let gid = nix::unistd::getgid().as_raw();

    // hello.txt exists in base; writing triggers COW.
    fs::write(s.mnt_path("hello.txt"), "cow\n").expect("write");
    s.cli(&["commit"]).expect("commit");

    let base_meta = fs::metadata(s.base_path("hello.txt")).unwrap();
    assert_eq!(
        base_meta.uid(),
        uid,
        "committed COW file should be owned by uid {uid}, got {}",
        base_meta.uid()
    );
    assert_eq!(
        base_meta.gid(),
        gid,
        "committed COW file should have gid {gid}, got {}",
        base_meta.gid()
    );
}

/// mkdir with specific mode through the mount, commit — mode preserved.
#[test]
fn commit_new_dir_with_chmod() {
    use std::os::unix::fs::MetadataExt;

    let s = YoloSession::new().expect("session setup");

    fs::create_dir(s.mnt_path("restricted")).expect("mkdir");
    fs::set_permissions(s.mnt_path("restricted"), fs::Permissions::from_mode(0o700))
        .expect("chmod dir");

    let mnt_meta = fs::metadata(s.mnt_path("restricted")).unwrap();
    assert_eq!(
        mnt_meta.mode() & 0o777,
        0o700,
        "mount should show 0700 for dir"
    );

    s.cli(&["commit"]).expect("commit");

    let base_meta = fs::metadata(s.base_path("restricted")).unwrap();
    assert_eq!(
        base_meta.mode() & 0o777,
        0o700,
        "committed dir should have mode 0700, got {:o}",
        base_meta.mode() & 0o777
    );
}

/// Create a new file, rename it, commit — the renamed file should appear in base.
#[test]
fn commit_rename_staged_only_file() {
    let s = YoloSession::new().expect("session setup");

    fs::write(s.mnt_path("staged.txt"), "staged\n").expect("write");
    fs::rename(s.mnt_path("staged.txt"), s.mnt_path("moved.txt")).expect("rename");

    let output = s.cli_stderr(&["commit"]).expect("commit");
    assert!(output.contains("committed"), "output: {output}");

    assert!(
        !s.base_path("staged.txt").exists(),
        "original staged name should not be in base"
    );
    assert_eq!(
        fs::read_to_string(s.base_path("moved.txt")).unwrap(),
        "staged\n",
        "renamed staged file should be in base"
    );
}

/// Directory permissions should be preserved when committing changes to children.
#[test]
fn commit_preserves_directory_mode() {
    use std::os::unix::fs::MetadataExt;

    let s = YoloSession::new().expect("session setup");

    // Set restricted permissions on the base directory
    let base_subdir = s.base_path("subdir");
    fs::set_permissions(&base_subdir, fs::Permissions::from_mode(0o750)).expect("chmod");

    // Modify a file inside the directory through the mount
    fs::write(s.mnt_path("subdir/deep.txt"), "modified\n").expect("write");
    s.cli(&["commit"]).expect("commit");

    let meta = fs::metadata(&base_subdir).unwrap();
    assert_eq!(
        meta.mode() & 0o777,
        0o750,
        "directory mode should be preserved, got {:o}",
        meta.mode() & 0o777
    );
    assert_eq!(
        fs::read_to_string(s.base_path("subdir/deep.txt")).unwrap(),
        "modified\n"
    );
}

/// Commit after travel should only apply live changes (snapshot state),
/// excluding dead-zone mutations.
#[test]
fn commit_after_travel_excludes_dead_zone() {
    let s = YoloSession::new().expect("session setup");

    fs::write(s.mnt_path("hello.txt"), "wanted\n").unwrap();
    fs::write(s.mnt_path("keep.txt"), "keep\n").unwrap();
    s.cli(&["snapshot", "chk1"]).expect("snapshot");

    // Post-snapshot changes (will be in dead zone after travel)
    fs::write(s.mnt_path("hello.txt"), "unwanted\n").unwrap();
    fs::write(s.mnt_path("dead.txt"), "dead\n").unwrap();
    fs::remove_file(s.mnt_path("keep.txt")).unwrap();

    s.cli(&["travel", "chk1"]).expect("travel");
    s.cli(&["commit"]).expect("commit");

    // Base should have snapshot state, not post-snapshot mutations
    assert_eq!(
        fs::read_to_string(s.base_path("hello.txt")).unwrap(),
        "wanted\n",
        "base should have snapshot content"
    );
    assert_eq!(
        fs::read_to_string(s.base_path("keep.txt")).unwrap(),
        "keep\n",
        "deleted-after-snapshot file should be in base"
    );
    assert!(
        !s.base_path("dead.txt").exists(),
        "post-snapshot file should NOT be in base"
    );
}

// ── DirTree commit plan edge cases ────────────────────────────────────

/// Create a file, modify it, commit. DirTree collapses to single Stage.
#[test]
fn commit_create_then_modify_collapses() {
    let s = YoloSession::new().expect("session setup");

    fs::write(s.mnt_path("new.txt"), "v1\n").unwrap();
    fs::write(s.mnt_path("new.txt"), "v2\n").unwrap();

    s.cli(&["commit"]).expect("commit");

    assert_eq!(
        fs::read_to_string(s.base_path("new.txt")).unwrap(),
        "v2\n",
        "should have the final version"
    );
}

/// Create a file then delete it. DirTree produces a tombstone, but since
/// the file never existed in base, the delete is a no-op.
#[test]
fn commit_create_then_delete_noop() {
    let s = YoloSession::new().expect("session setup");

    let orig_hello = fs::read_to_string(s.base_path("hello.txt")).unwrap();

    fs::write(s.mnt_path("ephemeral.txt"), "gone\n").unwrap();
    fs::remove_file(s.mnt_path("ephemeral.txt")).unwrap();

    s.cli(&["commit"]).expect("commit");

    assert!(
        !s.base_path("ephemeral.txt").exists(),
        "ephemeral file should not be in base"
    );
    // Existing files should be untouched
    assert_eq!(
        fs::read_to_string(s.base_path("hello.txt")).unwrap(),
        orig_hello
    );
}

/// Rename chain: a → b → c. DirTree collapses to single redirect c → a.
#[test]
fn commit_rename_chain() {
    let s = YoloSession::new().expect("session setup");

    fs::rename(s.mnt_path("hello.txt"), s.mnt_path("step1.txt")).unwrap();
    fs::rename(s.mnt_path("step1.txt"), s.mnt_path("step2.txt")).unwrap();

    s.cli(&["commit"]).expect("commit");

    assert!(
        !s.base_path("hello.txt").exists(),
        "original should be gone"
    );
    assert!(
        !s.base_path("step1.txt").exists(),
        "intermediate should be gone"
    );
    assert_eq!(
        fs::read_to_string(s.base_path("step2.txt")).unwrap(),
        "base content\n",
        "final name should have original content"
    );
}

/// Swap two files: a ↔ b using a temp.
#[test]
fn commit_swap_files() {
    let s = YoloSession::new().expect("session setup");

    let orig_hello = fs::read_to_string(s.mnt_path("hello.txt")).unwrap();
    let orig_multi = fs::read_to_string(s.mnt_path("multi.txt")).unwrap();

    fs::rename(s.mnt_path("hello.txt"), s.mnt_path("_tmp")).unwrap();
    fs::rename(s.mnt_path("multi.txt"), s.mnt_path("hello.txt")).unwrap();
    fs::rename(s.mnt_path("_tmp"), s.mnt_path("multi.txt")).unwrap();

    s.cli(&["commit"]).expect("commit");

    assert_eq!(
        fs::read_to_string(s.base_path("hello.txt")).unwrap(),
        orig_multi,
        "hello.txt should have multi.txt's original content"
    );
    assert_eq!(
        fs::read_to_string(s.base_path("multi.txt")).unwrap(),
        orig_hello,
        "multi.txt should have hello.txt's original content"
    );
}

/// 3-way rotation: a → b → c → a using a temp.
#[test]
fn commit_three_way_rotation() {
    let s = YoloSession::new().expect("session setup");

    // Create a third file
    fs::write(s.mnt_path("third.txt"), "third\n").unwrap();
    s.cli(&["commit"]).expect("commit setup");

    let orig_hello = fs::read_to_string(s.base_path("hello.txt")).unwrap();
    let orig_multi = fs::read_to_string(s.base_path("multi.txt")).unwrap();
    let orig_third = fs::read_to_string(s.base_path("third.txt")).unwrap();

    // Rotate: hello → multi → third → hello
    fs::rename(s.mnt_path("hello.txt"), s.mnt_path("_tmp")).unwrap();
    fs::rename(s.mnt_path("third.txt"), s.mnt_path("hello.txt")).unwrap();
    fs::rename(s.mnt_path("multi.txt"), s.mnt_path("third.txt")).unwrap();
    fs::rename(s.mnt_path("_tmp"), s.mnt_path("multi.txt")).unwrap();

    s.cli(&["commit"]).expect("commit");

    assert_eq!(
        fs::read_to_string(s.base_path("hello.txt")).unwrap(),
        orig_third
    );
    assert_eq!(
        fs::read_to_string(s.base_path("multi.txt")).unwrap(),
        orig_hello
    );
    assert_eq!(
        fs::read_to_string(s.base_path("third.txt")).unwrap(),
        orig_multi
    );
}

/// Rename a child out of a directory, then rename the parent.
/// Tests source-prefix dependency ordering.
#[test]
fn commit_extract_child_then_rename_parent() {
    let s = YoloSession::new().expect("session setup");

    fs::rename(s.mnt_path("subdir/deep.txt"), s.mnt_path("extracted.txt")).unwrap();
    fs::rename(s.mnt_path("subdir"), s.mnt_path("newdir")).unwrap();

    s.cli(&["commit"]).expect("commit");

    assert_eq!(
        fs::read_to_string(s.base_path("extracted.txt")).unwrap(),
        "nested\n"
    );
    assert!(s.base_path("newdir").is_dir());
    assert!(!s.base_path("subdir").exists());
}

/// Rename a directory, then create a file inside the renamed dir.
#[test]
fn commit_rename_dir_then_create_child() {
    let s = YoloSession::new().expect("session setup");

    fs::rename(s.mnt_path("subdir"), s.mnt_path("moved")).unwrap();
    fs::write(s.mnt_path("moved/new_child.txt"), "child\n").unwrap();

    s.cli(&["commit"]).expect("commit");

    assert!(s.base_path("moved").is_dir());
    assert!(!s.base_path("subdir").exists());
    assert_eq!(
        fs::read_to_string(s.base_path("moved/deep.txt")).unwrap(),
        "nested\n",
        "original child should follow directory rename"
    );
    assert_eq!(
        fs::read_to_string(s.base_path("moved/new_child.txt")).unwrap(),
        "child\n",
        "newly created child should be committed"
    );
}

/// Delete a directory tree, recreate it with different content.
#[test]
fn commit_delete_and_recreate_dir() {
    let s = YoloSession::new().expect("session setup");

    fs::remove_dir_all(s.mnt_path("subdir")).unwrap();
    fs::create_dir(s.mnt_path("subdir")).unwrap();
    fs::write(s.mnt_path("subdir/fresh.txt"), "fresh\n").unwrap();

    s.cli(&["commit"]).expect("commit");

    assert!(s.base_path("subdir").is_dir());
    assert!(
        !s.base_path("subdir/deep.txt").exists(),
        "old child should be gone"
    );
    assert_eq!(
        fs::read_to_string(s.base_path("subdir/fresh.txt")).unwrap(),
        "fresh\n"
    );
}

/// Create nested directories and files, commit all at once.
#[test]
fn commit_deep_nested_creation() {
    let s = YoloSession::new().expect("session setup");

    fs::create_dir_all(s.mnt_path("a/b/c")).unwrap();
    fs::write(s.mnt_path("a/top.txt"), "top\n").unwrap();
    fs::write(s.mnt_path("a/b/mid.txt"), "mid\n").unwrap();
    fs::write(s.mnt_path("a/b/c/deep.txt"), "deep\n").unwrap();

    s.cli(&["commit"]).expect("commit");

    assert_eq!(
        fs::read_to_string(s.base_path("a/top.txt")).unwrap(),
        "top\n"
    );
    assert_eq!(
        fs::read_to_string(s.base_path("a/b/mid.txt")).unwrap(),
        "mid\n"
    );
    assert_eq!(
        fs::read_to_string(s.base_path("a/b/c/deep.txt")).unwrap(),
        "deep\n"
    );
}

/// Rename into a new nested directory path.
#[test]
fn commit_rename_into_new_dir() {
    let s = YoloSession::new().expect("session setup");

    fs::create_dir_all(s.mnt_path("newdir")).unwrap();
    fs::rename(s.mnt_path("hello.txt"), s.mnt_path("newdir/hello.txt")).unwrap();

    s.cli(&["commit"]).expect("commit");

    assert!(!s.base_path("hello.txt").exists());
    assert_eq!(
        fs::read_to_string(s.base_path("newdir/hello.txt")).unwrap(),
        "base content\n"
    );
}

/// Multiple independent renames that don't conflict.
#[test]
fn commit_independent_renames() {
    let s = YoloSession::new().expect("session setup");

    fs::rename(s.mnt_path("hello.txt"), s.mnt_path("greeting.txt")).unwrap();
    fs::rename(s.mnt_path("multi.txt"), s.mnt_path("lines.txt")).unwrap();

    s.cli(&["commit"]).expect("commit");

    assert!(!s.base_path("hello.txt").exists());
    assert!(!s.base_path("multi.txt").exists());
    assert_eq!(
        fs::read_to_string(s.base_path("greeting.txt")).unwrap(),
        "base content\n"
    );
    assert_eq!(
        fs::read_to_string(s.base_path("lines.txt")).unwrap(),
        "line1\nline2\n"
    );
}

/// Rename a base file onto a newly staged file (overwrite staged with base).
#[test]
fn commit_rename_base_over_staged() {
    let s = YoloSession::new().expect("session setup");

    fs::write(s.mnt_path("target.txt"), "staged content\n").unwrap();
    fs::rename(s.mnt_path("hello.txt"), s.mnt_path("target.txt")).unwrap();

    s.cli(&["commit"]).expect("commit");

    assert!(!s.base_path("hello.txt").exists());
    assert_eq!(
        fs::read_to_string(s.base_path("target.txt")).unwrap(),
        "base content\n",
        "renamed base file should overwrite staged file"
    );
}

/// Mix of all operation types in one commit.
#[test]
fn commit_mixed_create_rename_delete() {
    let s = YoloSession::new().expect("session setup");

    // Create
    fs::write(s.mnt_path("new.txt"), "new\n").unwrap();
    fs::create_dir(s.mnt_path("newdir")).unwrap();
    fs::write(s.mnt_path("newdir/inside.txt"), "inside\n").unwrap();

    // Rename
    fs::rename(s.mnt_path("hello.txt"), s.mnt_path("moved.txt")).unwrap();

    // Delete
    fs::remove_file(s.mnt_path("multi.txt")).unwrap();

    // Modify
    fs::write(s.mnt_path("test.sh"), "#!/bin/sh\necho modified\n").unwrap();

    s.cli(&["commit"]).expect("commit");

    assert_eq!(fs::read_to_string(s.base_path("new.txt")).unwrap(), "new\n");
    assert_eq!(
        fs::read_to_string(s.base_path("newdir/inside.txt")).unwrap(),
        "inside\n"
    );
    assert_eq!(
        fs::read_to_string(s.base_path("moved.txt")).unwrap(),
        "base content\n"
    );
    assert!(!s.base_path("hello.txt").exists());
    assert!(!s.base_path("multi.txt").exists());
    assert_eq!(
        fs::read_to_string(s.base_path("test.sh")).unwrap(),
        "#!/bin/sh\necho modified\n"
    );
}

/// Commit twice in a row — second commit should work on a clean state.
#[test]
fn commit_twice() {
    let s = YoloSession::new().expect("session setup");

    fs::write(s.mnt_path("hello.txt"), "round1\n").unwrap();
    s.cli(&["commit"]).expect("commit 1");
    assert_eq!(
        fs::read_to_string(s.base_path("hello.txt")).unwrap(),
        "round1\n"
    );

    fs::write(s.mnt_path("hello.txt"), "round2\n").unwrap();
    s.cli(&["commit"]).expect("commit 2");
    assert_eq!(
        fs::read_to_string(s.base_path("hello.txt")).unwrap(),
        "round2\n"
    );
}

/// Commit with symlinks in a renamed directory.
#[test]
fn commit_rename_dir_with_symlink() {
    let s = YoloSession::new().expect("session setup");

    std::os::unix::fs::symlink("deep.txt", s.mnt_path("subdir/link")).unwrap();
    fs::rename(s.mnt_path("subdir"), s.mnt_path("moved")).unwrap();

    s.cli(&["commit"]).expect("commit");

    assert!(
        s.base_path("moved/link")
            .symlink_metadata()
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(
        fs::read_link(s.base_path("moved/link"))
            .unwrap()
            .to_string_lossy(),
        "deep.txt"
    );
}

/// Commit, then commit again with no changes — should report nothing.
#[test]
fn commit_then_noop_commit() {
    let s = YoloSession::new().expect("session setup");

    fs::write(s.mnt_path("hello.txt"), "changed\n").unwrap();
    s.cli(&["commit"]).expect("first commit");

    let output = s.cli_stderr(&["commit"]).expect("second commit");
    assert!(
        output.contains("nothing to commit"),
        "second commit should be a no-op, got: {output}"
    );
}

#[test]
fn commit_delete_dir_with_renamed_child() {
    let s = YoloSession::new().expect("session setup");

    // Rename a file out of subdir, then remove the subdir.
    fs::rename(s.mnt_path("subdir/deep.txt"), s.mnt_path("extracted.txt")).unwrap();
    fs::remove_dir(s.mnt_path("subdir")).unwrap();

    s.cli(&["commit"]).expect("commit");

    assert_eq!(
        fs::read_to_string(s.base_path("extracted.txt")).unwrap(),
        "nested\n",
        "renamed-out file should be committed"
    );
    assert!(
        !s.base_path("subdir").exists(),
        "deleted directory should be removed from base"
    );
}

#[test]
fn commit_rename_through_redirect() {
    // mv /subdir /other, then mv /other/deep.txt /out.txt.
    // The source /other/deep.txt is an overlay path that resolves to
    // base /subdir/deep.txt.  Commit must handle this correctly.
    let s = YoloSession::new().expect("session setup");

    fs::rename(s.mnt_path("subdir"), s.mnt_path("other")).unwrap();
    fs::rename(s.mnt_path("other/deep.txt"), s.mnt_path("out.txt")).unwrap();

    s.cli(&["commit"]).expect("commit");

    assert_eq!(
        fs::read_to_string(s.base_path("out.txt")).unwrap(),
        "nested\n",
        "file renamed through redirect should have original content"
    );
    assert!(
        s.base_path("other").exists(),
        "renamed directory should exist in base"
    );
    assert!(
        !s.base_path("other/deep.txt").exists(),
        "moved-out file should not remain in renamed directory"
    );
    assert!(
        !s.base_path("subdir").exists(),
        "original directory should be gone"
    );
}

#[test]
fn commit_rename_into_redirected_dir() {
    // mv /subdir /other, then mv /hello.txt /other/new.txt.
    // Commit must place both the renamed dir and the new child correctly.
    let s = YoloSession::new().expect("session setup");

    fs::rename(s.mnt_path("subdir"), s.mnt_path("other")).unwrap();
    fs::rename(s.mnt_path("hello.txt"), s.mnt_path("other/new.txt")).unwrap();

    s.cli(&["commit"]).expect("commit");

    assert_eq!(
        fs::read_to_string(s.base_path("other/deep.txt")).unwrap(),
        "nested\n",
        "original child should survive in renamed dir"
    );
    assert_eq!(
        fs::read_to_string(s.base_path("other/new.txt")).unwrap(),
        "base content\n",
        "file moved into renamed dir should have original content"
    );
    assert!(
        !s.base_path("subdir").exists(),
        "original directory should be gone"
    );
    assert!(
        !s.base_path("hello.txt").exists(),
        "moved file should be gone from original location"
    );
}

#[test]
fn commit_rename_dir_then_stage_child() {
    // mv /subdir /other, then create a new file in /other.
    // Rename must commit before the stage (phase ordering).
    let s = YoloSession::new().expect("session setup");

    fs::rename(s.mnt_path("subdir"), s.mnt_path("other")).unwrap();
    fs::write(s.mnt_path("other/new.txt"), "fresh\n").unwrap();

    s.cli(&["commit"]).expect("commit");

    assert_eq!(
        fs::read_to_string(s.base_path("other/deep.txt")).unwrap(),
        "nested\n",
        "original child should survive"
    );
    assert_eq!(
        fs::read_to_string(s.base_path("other/new.txt")).unwrap(),
        "fresh\n",
        "new file inside renamed dir should be committed"
    );
    assert!(!s.base_path("subdir").exists());
}

#[test]
fn commit_rename_dir_delete_child_stage_child() {
    // mv /subdir /other, rm /other/deep.txt, create /other/replacement.txt.
    // Exercises all three phases on one redirected directory.
    let s = YoloSession::new().expect("session setup");

    fs::rename(s.mnt_path("subdir"), s.mnt_path("other")).unwrap();
    fs::remove_file(s.mnt_path("other/deep.txt")).unwrap();
    fs::write(s.mnt_path("other/replacement.txt"), "new\n").unwrap();

    s.cli(&["commit"]).expect("commit");

    assert!(s.base_path("other").is_dir());
    assert!(
        !s.base_path("other/deep.txt").exists(),
        "deleted child should be gone"
    );
    assert_eq!(
        fs::read_to_string(s.base_path("other/replacement.txt")).unwrap(),
        "new\n",
    );
    assert!(!s.base_path("subdir").exists());
}

#[test]
fn commit_cross_rename_between_dirs() {
    // Move a file from one dir to another, and vice versa.
    // mv /hello.txt /subdir/moved_in.txt
    // mv /subdir/deep.txt /extracted.txt
    let s = YoloSession::new().expect("session setup");

    fs::rename(s.mnt_path("hello.txt"), s.mnt_path("subdir/moved_in.txt")).unwrap();
    fs::rename(s.mnt_path("subdir/deep.txt"), s.mnt_path("extracted.txt")).unwrap();

    s.cli(&["commit"]).expect("commit");

    assert_eq!(
        fs::read_to_string(s.base_path("subdir/moved_in.txt")).unwrap(),
        "base content\n",
    );
    assert_eq!(
        fs::read_to_string(s.base_path("extracted.txt")).unwrap(),
        "nested\n",
    );
    assert!(!s.base_path("hello.txt").exists());
    assert!(!s.base_path("subdir/deep.txt").exists());
}

#[test]
fn commit_multiple_renames_into_redirected_dir() {
    // mv /subdir /other, then mv multiple files into /other.
    // All destination-prefix orderings must be correct.
    let s = YoloSession::new().expect("session setup");

    fs::rename(s.mnt_path("subdir"), s.mnt_path("other")).unwrap();
    fs::rename(s.mnt_path("hello.txt"), s.mnt_path("other/a.txt")).unwrap();
    fs::rename(s.mnt_path("multi.txt"), s.mnt_path("other/b.txt")).unwrap();

    s.cli(&["commit"]).expect("commit");

    assert_eq!(
        fs::read_to_string(s.base_path("other/deep.txt")).unwrap(),
        "nested\n"
    );
    assert_eq!(
        fs::read_to_string(s.base_path("other/a.txt")).unwrap(),
        "base content\n"
    );
    assert_eq!(
        fs::read_to_string(s.base_path("other/b.txt")).unwrap(),
        "line1\nline2\n"
    );
    assert!(!s.base_path("subdir").exists());
    assert!(!s.base_path("hello.txt").exists());
    assert!(!s.base_path("multi.txt").exists());
}

#[test]
fn commit_rename_child_within_redirected_dir() {
    // mv /subdir /other, then mv /other/deep.txt /other/renamed.txt.
    // Creates a src-prefix vs dst-prefix cycle → needs temp rename.
    let s = YoloSession::new().expect("session setup");

    fs::rename(s.mnt_path("subdir"), s.mnt_path("other")).unwrap();
    fs::rename(
        s.mnt_path("other/deep.txt"),
        s.mnt_path("other/renamed.txt"),
    )
    .unwrap();

    s.cli(&["commit"]).expect("commit");

    assert_eq!(
        fs::read_to_string(s.base_path("other/renamed.txt")).unwrap(),
        "nested\n",
    );
    assert!(!s.base_path("other/deep.txt").exists());
    assert!(!s.base_path("subdir").exists());
}

#[test]
fn commit_deep_rename_chain_with_extraction() {
    // mv /subdir /a, mv /a /b, then mv /b/deep.txt /out.txt.
    // Chain collapses to /b ← /subdir. Source resolves /b/deep.txt ← /subdir/deep.txt.
    let s = YoloSession::new().expect("session setup");

    fs::rename(s.mnt_path("subdir"), s.mnt_path("a")).unwrap();
    fs::rename(s.mnt_path("a"), s.mnt_path("b")).unwrap();
    fs::rename(s.mnt_path("b/deep.txt"), s.mnt_path("out.txt")).unwrap();

    s.cli(&["commit"]).expect("commit");

    assert_eq!(
        fs::read_to_string(s.base_path("out.txt")).unwrap(),
        "nested\n",
    );
    assert!(s.base_path("b").is_dir(), "renamed dir should exist");
    assert!(
        !s.base_path("b/deep.txt").exists(),
        "extracted file should be gone from dir"
    );
    assert!(!s.base_path("subdir").exists());
    assert!(!s.base_path("a").exists());
}

#[test]
fn commit_roundtrip_rename_preserves_content() {
    // mv /hello.txt /tmp, mv /tmp /hello.txt — file ends up unchanged.
    // The intermediate /tmp path produces a spurious tombstone, but
    // hello.txt's content is preserved.
    let s = YoloSession::new().expect("session setup");

    fs::rename(s.mnt_path("hello.txt"), s.mnt_path("tmp")).unwrap();
    fs::rename(s.mnt_path("tmp"), s.mnt_path("hello.txt")).unwrap();

    s.cli(&["commit"]).expect("commit");

    assert_eq!(
        fs::read_to_string(s.base_path("hello.txt")).unwrap(),
        "base content\n",
        "roundtrip rename should preserve content"
    );
}

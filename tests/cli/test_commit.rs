use crate::helpers::AgfsSession;
use std::fs;
use std::os::unix::fs::PermissionsExt;

#[test]
fn commit_modified_file() {
    let s = AgfsSession::new().expect("session setup");
    s.run_in_namespace(|| {
        fs::write(s.mnt_path("hello.txt"), "committed\n").unwrap();

        let output = s.cli(&["commit"]).expect("commit");
        assert!(output.contains("Committed 1 change"), "output: {output}");

        // Base file now has the committed content
        assert_eq!(
            fs::read_to_string(s.base_path("hello.txt")).unwrap(),
            "committed\n"
        );
    });
}

#[test]
fn commit_new_file() {
    let s = AgfsSession::new().expect("session setup");
    s.run_in_namespace(|| {
        fs::write(s.mnt_path("brandnew.txt"), "new\n").unwrap();

        let output = s.cli(&["commit"]).expect("commit");
        assert!(output.contains("Committed 1 change"), "output: {output}");

        // New file now in base
        assert_eq!(
            fs::read_to_string(s.base_path("brandnew.txt")).unwrap(),
            "new\n"
        );
    });
}

#[test]
fn commit_multiple_changes() {
    let s = AgfsSession::new().expect("session setup");
    s.run_in_namespace(|| {
        fs::write(s.mnt_path("hello.txt"), "changed\n").unwrap();
        fs::write(s.mnt_path("newfile.txt"), "new\n").unwrap();

        let output = s.cli(&["commit"]).expect("commit");
        assert!(output.contains("Committed 2 change"), "output: {output}");

        assert_eq!(
            fs::read_to_string(s.base_path("hello.txt")).unwrap(),
            "changed\n"
        );
        assert_eq!(
            fs::read_to_string(s.base_path("newfile.txt")).unwrap(),
            "new\n"
        );
    });
}

#[test]
fn commit_nothing() {
    let s = AgfsSession::new().expect("session setup");
    s.run_in_namespace(|| {
        let output = s.cli(&["commit"]).expect("commit");
        assert!(output.contains("Nothing to commit"), "output: {output}");
    });
}

/// Delete a directory, create a file with the same name, commit.
/// The commit should replace the directory with the file in base.
#[test]
fn commit_replace_dir_with_file() {
    let s = AgfsSession::new().expect("session setup");
    s.run_in_namespace(|| {
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
    });
}

/// Commit after rename: the renamed file should appear at its new location in base.
#[test]
fn commit_rename_file() {
    let s = AgfsSession::new().expect("session setup");
    s.run_in_namespace(|| {
        // Rename an existing base file through the mount
        fs::rename(s.mnt_path("hello.txt"), s.mnt_path("greeting.txt")).expect("rename");

        let output = s.cli(&["commit"]).expect("commit");
        assert!(output.contains("Committed"), "output: {output}");

        assert!(
            !s.base_path("hello.txt").exists(),
            "original should be gone from base"
        );
        assert_eq!(
            fs::read_to_string(s.base_path("greeting.txt")).unwrap(),
            "base content\n",
            "renamed file should have original content in base"
        );
    });
}

/// Commit after renaming a directory: children should follow.
#[test]
fn commit_rename_directory() {
    let s = AgfsSession::new().expect("session setup");
    s.run_in_namespace(|| {
        // subdir/ exists in base with subdir/deep.txt
        fs::rename(s.mnt_path("subdir"), s.mnt_path("renamed_dir")).expect("rename dir");

        let output = s.cli(&["commit"]).expect("commit");
        assert!(output.contains("Committed"), "output: {output}");

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
    });
}

/// Commit after deleting a directory: the directory and its children should be removed from base.
#[test]
fn commit_delete_directory() {
    let s = AgfsSession::new().expect("session setup");
    s.run_in_namespace(|| {
        // subdir/ exists in base with subdir/deep.txt
        fs::remove_dir_all(s.mnt_path("subdir")).expect("rmdir");

        let output = s.cli(&["commit"]).expect("commit");
        assert!(output.contains("Committed"), "output: {output}");

        assert!(
            !s.base_path("subdir").exists(),
            "deleted dir should be gone from base"
        );
        assert!(
            !s.base_path("subdir/deep.txt").exists(),
            "children should be gone from base"
        );
    });
}

#[test]
fn commit_preserves_readonly_mode() {
    use std::os::unix::fs::MetadataExt;

    let s = AgfsSession::new().expect("session setup");
    s.run_in_namespace(|| {
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
    });
}

/// Committing a modified executable file should preserve its 0o755 mode.
#[test]
fn commit_preserves_executable_mode() {
    use std::os::unix::fs::MetadataExt;

    let s = AgfsSession::new().expect("session setup");
    s.run_in_namespace(|| {
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
    });
}

/// Committing a newly created file should keep its default mode (not
/// inherit from some other file).
#[test]
fn commit_new_file_has_default_mode() {
    use std::os::unix::fs::MetadataExt;

    let s = AgfsSession::new().expect("session setup");
    s.run_in_namespace(|| {
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
    });
}

/// Rename a file onto another existing file (Replace record).
/// The destination should be overwritten with the source content.
#[test]
fn commit_rename_replace() {
    let s = AgfsSession::new().expect("session setup");
    s.run_in_namespace(|| {
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
    });
}

/// Rename a directory onto another existing directory.
/// Children from both should be merged.
#[test]
fn commit_rename_dir_onto_dir() {
    let s = AgfsSession::new().expect("session setup");
    s.run_in_namespace(|| {
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
    });
}

/// Create a symlink through the mount, commit, verify it appears in base.
#[test]
fn commit_symlink() {
    let s = AgfsSession::new().expect("session setup");
    s.run_in_namespace(|| {
        std::os::unix::fs::symlink("hello.txt", s.mnt_path("link.txt")).expect("symlink");

        let output = s.cli(&["commit"]).expect("commit");
        assert!(output.contains("Committed"), "output: {output}");

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
    });
}

/// Modify a symlink's target, commit, verify the new target in base.
#[test]
fn commit_modified_symlink() {
    let s = AgfsSession::new().expect("session setup");
    s.run_in_namespace(|| {
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
    });
}

/// Create a new file, rename it, commit — the renamed file should appear in base.
#[test]
fn commit_rename_staged_only_file() {
    let s = AgfsSession::new().expect("session setup");
    s.run_in_namespace(|| {
        fs::write(s.mnt_path("staged.txt"), "staged\n").expect("write");
        fs::rename(s.mnt_path("staged.txt"), s.mnt_path("moved.txt")).expect("rename");

        let output = s.cli(&["commit"]).expect("commit");
        assert!(output.contains("Committed"), "output: {output}");

        assert!(
            !s.base_path("staged.txt").exists(),
            "original staged name should not be in base"
        );
        assert_eq!(
            fs::read_to_string(s.base_path("moved.txt")).unwrap(),
            "staged\n",
            "renamed staged file should be in base"
        );
    });
}

/// Directory permissions should be preserved when committing changes to children.
#[test]
fn commit_preserves_directory_mode() {
    use std::os::unix::fs::MetadataExt;

    let s = AgfsSession::new().expect("session setup");
    s.run_in_namespace(|| {
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
    });
}

/// Commit after restore should only apply live changes (checkpoint state),
/// excluding dead-zone mutations.
#[test]
fn commit_after_restore_excludes_dead_zone() {
    let s = AgfsSession::new().expect("session setup");
    s.run_in_namespace(|| {
        fs::write(s.mnt_path("hello.txt"), "wanted\n").unwrap();
        fs::write(s.mnt_path("keep.txt"), "keep\n").unwrap();
        s.cli(&["checkpoint", "chk1"]).expect("checkpoint");

        // Post-checkpoint changes (will be in dead zone after restore)
        fs::write(s.mnt_path("hello.txt"), "unwanted\n").unwrap();
        fs::write(s.mnt_path("dead.txt"), "dead\n").unwrap();
        fs::remove_file(s.mnt_path("keep.txt")).unwrap();

        s.cli(&["restore", "chk1"]).expect("restore");
        s.cli(&["commit"]).expect("commit");

        // Base should have checkpoint state, not post-checkpoint mutations
        assert_eq!(
            fs::read_to_string(s.base_path("hello.txt")).unwrap(),
            "wanted\n",
            "base should have checkpoint content"
        );
        assert_eq!(
            fs::read_to_string(s.base_path("keep.txt")).unwrap(),
            "keep\n",
            "deleted-after-checkpoint file should be in base"
        );
        assert!(
            !s.base_path("dead.txt").exists(),
            "post-checkpoint file should NOT be in base"
        );
    });
}

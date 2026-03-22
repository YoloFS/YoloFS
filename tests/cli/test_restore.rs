use crate::helpers::AgfsSession;
use std::fs;

/// Restore to a checkpoint makes the mount reflect the checkpoint state.
#[test]
fn restore_shows_checkpoint_state() {
    let s = AgfsSession::new().expect("session setup");
    s.run_in_namespace(|| {
        fs::write(s.mnt_path("hello.txt"), "version 1\n").expect("write v1");
        s.cli(&["checkpoint", "chk1"]).expect("checkpoint");

        fs::write(s.mnt_path("hello.txt"), "version 2\n").expect("write v2");

        s.cli(&["restore", "chk1"]).expect("restore");

        let content = fs::read_to_string(s.mnt_path("hello.txt")).expect("read");
        assert_eq!(content, "version 1\n", "should see checkpoint state");
    });
}

/// Post-checkpoint created files are invisible after restore.
#[test]
fn restore_hides_post_checkpoint_creates() {
    let s = AgfsSession::new().expect("session setup");
    s.run_in_namespace(|| {
        fs::write(s.mnt_path("before.txt"), "before\n").expect("write before");
        s.cli(&["checkpoint", "chk1"]).expect("checkpoint");

        fs::write(s.mnt_path("after.txt"), "after\n").expect("write after");
        assert!(s.mnt_path("after.txt").exists());

        s.cli(&["restore", "chk1"]).expect("restore");

        assert!(
            s.mnt_path("before.txt").exists(),
            "pre-checkpoint file should be visible"
        );
        assert!(
            !s.mnt_path("after.txt").exists(),
            "post-checkpoint file should be hidden"
        );
    });
}

/// Post-checkpoint deletes are undone after restore.
#[test]
fn restore_undoes_post_checkpoint_deletes() {
    let s = AgfsSession::new().expect("session setup");
    s.run_in_namespace(|| {
        fs::write(s.mnt_path("doomed.txt"), "keep me\n").expect("write");
        s.cli(&["checkpoint", "chk1"]).expect("checkpoint");

        fs::remove_file(s.mnt_path("doomed.txt")).expect("delete");
        assert!(!s.mnt_path("doomed.txt").exists());

        s.cli(&["restore", "chk1"]).expect("restore");

        assert!(
            s.mnt_path("doomed.txt").exists(),
            "deleted file should reappear after restore"
        );
        let content = fs::read_to_string(s.mnt_path("doomed.txt")).expect("read");
        assert_eq!(content, "keep me\n");
    });
}

/// COW works after restore — editing a restored file creates a new inode.
#[test]
fn cow_works_after_restore() {
    let s = AgfsSession::new().expect("session setup");
    s.run_in_namespace(|| {
        fs::write(s.mnt_path("hello.txt"), "v1\n").expect("write v1");
        s.cli(&["checkpoint", "chk1"]).expect("checkpoint 1");

        fs::write(s.mnt_path("hello.txt"), "v2\n").expect("write v2");
        s.cli(&["checkpoint", "chk2"]).expect("checkpoint 2");

        s.cli(&["restore", "chk1"]).expect("restore to chk1");

        // Take a new checkpoint so writes trigger re-COW
        s.cli(&["checkpoint", "post-restore"])
            .expect("checkpoint after restore");

        // Write should work (triggers re-COW from the restored inode)
        fs::write(s.mnt_path("hello.txt"), "v3\n").expect("write v3");
        let content = fs::read_to_string(s.mnt_path("hello.txt")).expect("read");
        assert_eq!(content, "v3\n");
    });
}

/// Commit after restore applies the restored state to base.
#[test]
fn commit_after_restore() {
    let s = AgfsSession::new().expect("session setup");
    s.run_in_namespace(|| {
        fs::write(s.mnt_path("hello.txt"), "wanted\n").expect("write");
        s.cli(&["checkpoint", "chk1"]).expect("checkpoint");

        fs::write(s.mnt_path("hello.txt"), "unwanted\n").expect("overwrite");

        s.cli(&["restore", "chk1"]).expect("restore");
        s.cli(&["commit"]).expect("commit");

        let base_content = fs::read_to_string(s.base_path("hello.txt")).expect("read base");
        assert_eq!(
            base_content, "wanted\n",
            "base should have restored content"
        );
    });
}

/// Restore with renames preserves the rename.
#[test]
fn restore_preserves_renames() {
    let s = AgfsSession::new().expect("session setup");
    s.run_in_namespace(|| {
        fs::write(s.mnt_path("old.txt"), "content\n").expect("write");
        fs::rename(s.mnt_path("old.txt"), s.mnt_path("new.txt")).expect("rename");
        s.cli(&["checkpoint", "chk1"]).expect("checkpoint");

        // Do something after checkpoint
        fs::write(s.mnt_path("extra.txt"), "extra\n").expect("write extra");

        s.cli(&["restore", "chk1"]).expect("restore");

        assert!(!s.mnt_path("old.txt").exists(), "old name should not exist");
        assert!(s.mnt_path("new.txt").exists(), "new name should exist");
        assert!(
            !s.mnt_path("extra.txt").exists(),
            "post-checkpoint file should be hidden"
        );
    });
}

/// Restore by numeric ID works.
#[test]
fn restore_by_numeric_id() {
    let s = AgfsSession::new().expect("session setup");
    s.run_in_namespace(|| {
        fs::write(s.mnt_path("hello.txt"), "v1\n").expect("write");
        // Checkpoint gets id=1 (first user checkpoint)
        s.cli(&["checkpoint", "chk"]).expect("checkpoint");

        fs::write(s.mnt_path("hello.txt"), "v2\n").expect("write v2");

        s.cli(&["restore", "1"]).expect("restore by id");

        let content = fs::read_to_string(s.mnt_path("hello.txt")).expect("read");
        assert_eq!(content, "v1\n");
    });
}

/// Restore CLI output includes checkpoint name and change count.
#[test]
fn restore_prints_summary() {
    let s = AgfsSession::new().expect("session setup");
    s.run_in_namespace(|| {
        fs::write(s.mnt_path("a.txt"), "a\n").expect("write a");
        fs::write(s.mnt_path("b.txt"), "b\n").expect("write b");
        s.cli(&["checkpoint", "two-files"]).expect("checkpoint");

        fs::write(s.mnt_path("c.txt"), "c\n").expect("write c");

        let output = s.cli(&["restore", "two-files"]).expect("restore");
        assert!(
            output.contains("two-files"),
            "output should mention checkpoint name: {output}"
        );
        assert!(
            output.contains("2"),
            "output should mention change count: {output}"
        );
    });
}

/// Restore to nonexistent checkpoint fails.
#[test]
fn restore_nonexistent_fails() {
    let s = AgfsSession::new().expect("session setup");
    s.run_in_namespace(|| {
        let result = s.cli(&["restore", "nonexistent"]);
        assert!(
            result.is_err(),
            "restoring to nonexistent checkpoint should fail"
        );
    });
}

// ── Complex multi-step scenarios ──────────────────────────────────────────

/// Restore to chk2, then restore further back to chk1.
#[test]
fn restore_backward_through_checkpoints() {
    let s = AgfsSession::new().expect("session setup");
    s.run_in_namespace(|| {
        fs::write(s.mnt_path("file.txt"), "v1\n").expect("write v1");
        s.cli(&["checkpoint", "chk1"]).expect("checkpoint 1");

        fs::write(s.mnt_path("file.txt"), "v2\n").expect("write v2");
        fs::write(s.mnt_path("extra.txt"), "extra\n").expect("write extra");
        s.cli(&["checkpoint", "chk2"]).expect("checkpoint 2");

        fs::write(s.mnt_path("file.txt"), "v3\n").expect("write v3");

        // Restore to chk2
        s.cli(&["restore", "chk2"]).expect("restore to chk2");
        assert_eq!(fs::read_to_string(s.mnt_path("file.txt")).unwrap(), "v2\n");
        assert!(s.mnt_path("extra.txt").exists());

        // Restore further back to chk1
        s.cli(&["restore", "chk1"]).expect("restore to chk1");
        assert_eq!(fs::read_to_string(s.mnt_path("file.txt")).unwrap(), "v1\n");
        assert!(
            !s.mnt_path("extra.txt").exists(),
            "extra.txt was created after chk1"
        );
    });
}

/// Restore, make new changes, checkpoint, restore to the new checkpoint.
#[test]
fn restore_edit_checkpoint_restore_cycle() {
    let s = AgfsSession::new().expect("session setup");
    s.run_in_namespace(|| {
        fs::write(s.mnt_path("file.txt"), "original\n").expect("write");
        s.cli(&["checkpoint", "chk1"]).expect("checkpoint 1");

        fs::write(s.mnt_path("file.txt"), "bad change\n").expect("overwrite");
        s.cli(&["checkpoint", "chk2"]).expect("checkpoint 2");

        // Restore to chk1
        s.cli(&["restore", "chk1"]).expect("restore to chk1");
        assert_eq!(
            fs::read_to_string(s.mnt_path("file.txt")).unwrap(),
            "original\n"
        );

        // Make new edits and checkpoint
        s.cli(&["checkpoint", "post-restore"])
            .expect("checkpoint post-restore");
        fs::write(s.mnt_path("file.txt"), "new version\n").expect("write new");
        s.cli(&["checkpoint", "chk3"]).expect("checkpoint 3");

        fs::write(s.mnt_path("file.txt"), "throwaway\n").expect("write throwaway");

        // Restore to chk3
        s.cli(&["restore", "chk3"]).expect("restore to chk3");
        assert_eq!(
            fs::read_to_string(s.mnt_path("file.txt")).unwrap(),
            "new version\n"
        );
    });
}

/// Restore with mkdir + files inside the directory.
#[test]
fn restore_with_nested_directory() {
    let s = AgfsSession::new().expect("session setup");
    s.run_in_namespace(|| {
        fs::create_dir(s.mnt_path("mydir")).expect("mkdir");
        fs::write(s.mnt_path("mydir/a.txt"), "a\n").expect("write a");
        fs::write(s.mnt_path("mydir/b.txt"), "b\n").expect("write b");
        s.cli(&["checkpoint", "chk1"]).expect("checkpoint");

        // Delete the directory contents after checkpoint
        fs::remove_file(s.mnt_path("mydir/a.txt")).expect("rm a");
        fs::write(s.mnt_path("mydir/c.txt"), "c\n").expect("write c");

        s.cli(&["restore", "chk1"]).expect("restore");

        assert!(s.mnt_path("mydir/a.txt").exists(), "a.txt should be back");
        assert!(s.mnt_path("mydir/b.txt").exists(), "b.txt should exist");
        assert!(
            !s.mnt_path("mydir/c.txt").exists(),
            "c.txt was post-checkpoint"
        );
    });
}

/// Restore with rename chain: mv a→b, mv b→c, checkpoint, then restore.
#[test]
fn restore_rename_chain() {
    let s = AgfsSession::new().expect("session setup");
    s.run_in_namespace(|| {
        fs::write(s.mnt_path("a.txt"), "content\n").expect("write a");
        fs::rename(s.mnt_path("a.txt"), s.mnt_path("b.txt")).expect("rename a→b");
        fs::rename(s.mnt_path("b.txt"), s.mnt_path("c.txt")).expect("rename b→c");
        s.cli(&["checkpoint", "chk1"]).expect("checkpoint");

        // Modify c after checkpoint
        fs::write(s.mnt_path("c.txt"), "changed\n").expect("modify c");

        s.cli(&["restore", "chk1"]).expect("restore");

        assert!(!s.mnt_path("a.txt").exists(), "a.txt renamed away");
        assert!(!s.mnt_path("b.txt").exists(), "b.txt renamed away");
        assert!(s.mnt_path("c.txt").exists(), "c.txt should exist");
        let content = fs::read_to_string(s.mnt_path("c.txt")).expect("read c");
        assert_eq!(content, "content\n", "should have original content");
    });
}

/// Restore then commit verifies the full round-trip: create → checkpoint →
/// more changes → restore → commit applies only checkpoint state to base.
#[test]
fn restore_then_commit_full_roundtrip() {
    let s = AgfsSession::new().expect("session setup");
    s.run_in_namespace(|| {
        // Create multiple files
        fs::write(s.mnt_path("keep.txt"), "keep\n").expect("write keep");
        fs::write(s.mnt_path("modify.txt"), "v1\n").expect("write modify");
        s.cli(&["checkpoint", "chk1"]).expect("checkpoint");

        // Post-checkpoint: modify one, delete another, create new
        fs::write(s.mnt_path("modify.txt"), "v2\n").expect("modify");
        fs::remove_file(s.mnt_path("keep.txt")).expect("delete keep");
        fs::write(s.mnt_path("new.txt"), "new\n").expect("create new");

        // Restore to chk1 and commit
        s.cli(&["restore", "chk1"]).expect("restore");
        s.cli(&["commit"]).expect("commit");

        // Base should have chk1 state
        assert_eq!(
            fs::read_to_string(s.base_path("keep.txt")).unwrap(),
            "keep\n"
        );
        assert_eq!(
            fs::read_to_string(s.base_path("modify.txt")).unwrap(),
            "v1\n"
        );
        assert!(
            !s.base_path("new.txt").exists(),
            "post-checkpoint file should not be in base"
        );
    });
}

/// Symlink at checkpoint is preserved after restore.
#[test]
fn restore_preserves_symlinks() {
    let s = AgfsSession::new().expect("session setup");
    s.run_in_namespace(|| {
        fs::write(s.mnt_path("target.txt"), "target\n").expect("write target");
        std::os::unix::fs::symlink("target.txt", s.mnt_path("link.txt")).expect("symlink");
        s.cli(&["checkpoint", "chk1"]).expect("checkpoint");

        fs::remove_file(s.mnt_path("link.txt")).expect("remove link");

        s.cli(&["restore", "chk1"]).expect("restore");

        assert!(
            s.mnt_path("link.txt").exists(),
            "symlink should exist after restore"
        );
        let content = fs::read_to_string(s.mnt_path("link.txt")).expect("read through link");
        assert_eq!(content, "target\n");
    });
}

/// Many files across many directories — stress test for dirent injection.
#[test]
fn restore_many_files_across_dirs() {
    let s = AgfsSession::new().expect("session setup");
    s.run_in_namespace(|| {
        // Create a tree with 3 dirs × 5 files = 15 entries
        for dir_i in 0..3 {
            let dir_name = format!("dir{dir_i}");
            fs::create_dir(s.mnt_path(&dir_name)).expect("mkdir");
            for file_i in 0..5 {
                let path = format!("{dir_name}/f{file_i}.txt");
                fs::write(s.mnt_path(&path), format!("{dir_i}-{file_i}\n")).expect("write");
            }
        }
        s.cli(&["checkpoint", "chk1"]).expect("checkpoint");

        // Wreak havoc after checkpoint
        fs::remove_file(s.mnt_path("dir0/f0.txt")).expect("delete");
        fs::write(s.mnt_path("dir1/f1.txt"), "overwritten\n").expect("overwrite");
        fs::write(s.mnt_path("dir2/new.txt"), "new\n").expect("create");

        s.cli(&["restore", "chk1"]).expect("restore");

        // Verify all 15 original files are intact
        for dir_i in 0..3 {
            for file_i in 0..5 {
                let path = format!("dir{dir_i}/f{file_i}.txt");
                let content = fs::read_to_string(s.mnt_path(&path))
                    .unwrap_or_else(|e| panic!("{path} should exist: {e}"));
                assert_eq!(content, format!("{dir_i}-{file_i}\n"), "content of {path}");
            }
        }
        assert!(
            !s.mnt_path("dir2/new.txt").exists(),
            "post-checkpoint file should be hidden"
        );
    });
}

// ── Interaction with other CLI commands ───────────────────────────────────

/// Readdir after restore shows the correct merged listing.
#[test]
fn readdir_after_restore() {
    let s = AgfsSession::new().expect("session setup");
    s.run_in_namespace(|| {
        fs::write(s.mnt_path("staged.txt"), "staged\n").expect("write staged");
        fs::write(s.mnt_path("deleted_base.txt"), "to delete\n").expect("write base file");
        // base already has files; staged.txt is new, deleted_base.txt will be deleted
        s.cli(&["checkpoint", "chk1"]).expect("checkpoint");

        fs::remove_file(s.mnt_path("staged.txt")).expect("remove staged");
        fs::write(s.mnt_path("post.txt"), "post\n").expect("write post");

        s.cli(&["restore", "chk1"]).expect("restore");

        let entries: Vec<String> = fs::read_dir(s.mnt_path("."))
            .expect("readdir")
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();

        assert!(
            entries.contains(&"staged.txt".to_string()),
            "staged.txt should appear in readdir: {entries:?}"
        );
        assert!(
            !entries.contains(&"post.txt".to_string()),
            "post.txt should NOT appear in readdir: {entries:?}"
        );
    });
}

/// `agfs status` after restore shows only checkpoint-state changes.
#[test]
fn status_after_restore() {
    let s = AgfsSession::new().expect("session setup");
    s.run_in_namespace(|| {
        fs::write(s.mnt_path("a.txt"), "a\n").expect("write a");
        s.cli(&["checkpoint", "chk1"]).expect("checkpoint");
        fs::write(s.mnt_path("b.txt"), "b\n").expect("write b");

        s.cli(&["restore", "chk1"]).expect("restore");

        let status = s.cli(&["status"]).expect("status");
        assert!(
            status.contains("a.txt"),
            "a.txt should be in status: {status}"
        );
        assert!(
            !status.contains("b.txt"),
            "b.txt should NOT be in status: {status}"
        );
    });
}

/// `agfs diff` after restore shows only checkpoint-state diffs.
#[test]
fn diff_after_restore() {
    let s = AgfsSession::new().expect("session setup");
    s.run_in_namespace(|| {
        fs::write(s.mnt_path("a.txt"), "a\n").expect("write a");
        s.cli(&["checkpoint", "chk1"]).expect("checkpoint");
        fs::write(s.mnt_path("b.txt"), "b\n").expect("write b");

        s.cli(&["restore", "chk1"]).expect("restore");

        let diff = s.cli(&["diff"]).expect("diff");
        assert!(diff.contains("a.txt"), "a.txt should be in diff: {diff}");
        assert!(
            !diff.contains("b.txt"),
            "b.txt should NOT be in diff: {diff}"
        );
    });
}

/// New mutations after restore append to the journal correctly.
#[test]
fn journal_appends_after_restore() {
    let s = AgfsSession::new().expect("session setup");
    s.run_in_namespace(|| {
        fs::write(s.mnt_path("old.txt"), "old\n").expect("write old");
        s.cli(&["checkpoint", "chk1"]).expect("checkpoint");
        fs::write(s.mnt_path("gone.txt"), "gone\n").expect("write gone");

        s.cli(&["restore", "chk1"]).expect("restore");

        // New mutation after restore
        fs::write(s.mnt_path("new.txt"), "new\n").expect("write new after restore");

        let status = s.cli(&["status"]).expect("status");
        assert!(
            status.contains("old.txt"),
            "pre-checkpoint file in status: {status}"
        );
        assert!(
            status.contains("new.txt"),
            "post-restore file in status: {status}"
        );
        assert!(
            !status.contains("gone.txt"),
            "discarded file NOT in status: {status}"
        );
    });
}

/// Restoring to the same checkpoint twice is idempotent.
#[test]
fn restore_idempotent() {
    let s = AgfsSession::new().expect("session setup");
    s.run_in_namespace(|| {
        fs::write(s.mnt_path("file.txt"), "v1\n").expect("write");
        s.cli(&["checkpoint", "chk1"]).expect("checkpoint");
        fs::write(s.mnt_path("file.txt"), "v2\n").expect("overwrite");

        s.cli(&["restore", "chk1"]).expect("restore 1");
        assert_eq!(fs::read_to_string(s.mnt_path("file.txt")).unwrap(), "v1\n");

        s.cli(&["restore", "chk1"]).expect("restore 2");
        assert_eq!(
            fs::read_to_string(s.mnt_path("file.txt")).unwrap(),
            "v1\n",
            "second restore should produce same state"
        );
    });
}

/// Deleting a base file before checkpoint; restore brings back the delete.
#[test]
fn restore_base_file_deletion() {
    let s = AgfsSession::new().expect("session setup");
    s.run_in_namespace(|| {
        // base_file.txt exists in the base FS (it was there before mount)
        // Write to create it through the mount, commit so it's in base, remount
        // Simpler: just delete a file that exists in base
        // The session root itself has files from the test harness.
        // Let's create, commit to base, then work with it.
        fs::write(s.mnt_path("base_file.txt"), "base\n").expect("write");
        s.cli(&["commit"]).expect("commit to base");

        // Now base_file.txt is in base. Delete it through the mount.
        fs::remove_file(s.mnt_path("base_file.txt")).expect("delete base file");
        assert!(!s.mnt_path("base_file.txt").exists());

        s.cli(&["checkpoint", "chk1"]).expect("checkpoint");

        // Recreate it after checkpoint
        fs::write(s.mnt_path("base_file.txt"), "recreated\n").expect("recreate");

        s.cli(&["restore", "chk1"]).expect("restore");

        // After restore, the delete should be active — file should be gone
        assert!(
            !s.mnt_path("base_file.txt").exists(),
            "deleted base file should stay deleted after restore"
        );
    });
}

// ── Edge cases ───────────────────────────────────────────────────────────

/// Deeply nested new directories: mkdir -p a/b/c with a file inside.
#[test]
fn restore_deeply_nested_new_dirs() {
    let s = AgfsSession::new().expect("session setup");
    s.run_in_namespace(|| {
        fs::create_dir(s.mnt_path("a")).expect("mkdir a");
        fs::create_dir(s.mnt_path("a/b")).expect("mkdir a/b");
        fs::create_dir(s.mnt_path("a/b/c")).expect("mkdir a/b/c");
        fs::write(s.mnt_path("a/b/c/deep.txt"), "deep\n").expect("write deep");
        s.cli(&["checkpoint", "chk1"]).expect("checkpoint");

        fs::write(s.mnt_path("a/b/c/deep.txt"), "overwritten\n").expect("overwrite");

        s.cli(&["restore", "chk1"]).expect("restore");

        assert_eq!(
            fs::read_to_string(s.mnt_path("a/b/c/deep.txt")).unwrap(),
            "deep\n",
            "deeply nested file should have checkpoint content"
        );
    });
}

/// Rename + recreate at the original name, then checkpoint and restore.
#[test]
fn restore_rename_then_recreate() {
    let s = AgfsSession::new().expect("session setup");
    s.run_in_namespace(|| {
        fs::write(s.mnt_path("a.txt"), "original\n").expect("write a");
        fs::rename(s.mnt_path("a.txt"), s.mnt_path("b.txt")).expect("rename a→b");
        fs::write(s.mnt_path("a.txt"), "new a\n").expect("recreate a");
        s.cli(&["checkpoint", "chk1"]).expect("checkpoint");

        // Mess things up after checkpoint
        fs::remove_file(s.mnt_path("a.txt")).expect("delete a");
        fs::remove_file(s.mnt_path("b.txt")).expect("delete b");

        s.cli(&["restore", "chk1"]).expect("restore");

        // Both a.txt (new) and b.txt (renamed from original a) should exist
        assert_eq!(fs::read_to_string(s.mnt_path("a.txt")).unwrap(), "new a\n");
        assert_eq!(
            fs::read_to_string(s.mnt_path("b.txt")).unwrap(),
            "original\n"
        );
    });
}

/// `agfs timeline` after restore shows all checkpoints and the restore record.
#[test]
fn timeline_after_restore() {
    let s = AgfsSession::new().expect("session setup");
    s.run_in_namespace(|| {
        fs::write(s.mnt_path("file.txt"), "v1\n").expect("write");
        s.cli(&["checkpoint", "first"]).expect("checkpoint 1");
        fs::write(s.mnt_path("file.txt"), "v2\n").expect("write");
        s.cli(&["checkpoint", "second"]).expect("checkpoint 2");

        s.cli(&["restore", "first"]).expect("restore");

        let timeline = s.cli(&["timeline"]).expect("timeline");
        assert!(
            timeline.contains("first"),
            "first should be in timeline: {timeline}"
        );
        assert!(
            timeline.contains("second"),
            "second should still be in timeline (append-only journal): {timeline}"
        );
        assert!(
            timeline.contains("restore"),
            "restore record should be in timeline: {timeline}"
        );
    });
}

/// Write after restore without new checkpoint modifies the file in place.
#[test]
fn write_after_restore_modifies_in_place() {
    let s = AgfsSession::new().expect("session setup");
    s.run_in_namespace(|| {
        fs::write(s.mnt_path("file.txt"), "v1\n").expect("write v1");
        s.cli(&["checkpoint", "chk1"]).expect("checkpoint");

        fs::write(s.mnt_path("file.txt"), "v2\n").expect("write v2");
        s.cli(&["restore", "chk1"]).expect("restore");

        // Write without taking a new checkpoint — should modify in place
        fs::write(s.mnt_path("file.txt"), "v1-edited\n").expect("edit after restore");

        let content = fs::read_to_string(s.mnt_path("file.txt")).expect("read");
        assert_eq!(content, "v1-edited\n");

        // Commit should apply the edited version
        s.cli(&["commit"]).expect("commit");
        let base = fs::read_to_string(s.base_path("file.txt")).expect("read base");
        assert_eq!(base, "v1-edited\n");
    });
}

/// Renamed directory has correct d_type (DT_DIR) after restore.
#[test]
fn restore_renamed_directory_dtype() {
    let s = AgfsSession::new().expect("session setup");
    s.run_in_namespace(|| {
        // Create directory in base first via commit
        fs::create_dir(s.mnt_path("old_dir")).expect("mkdir");
        fs::write(s.mnt_path("old_dir/file.txt"), "content\n").expect("write");
        s.cli(&["commit"]).expect("commit to base");

        // Now rename the base directory through the mount
        fs::rename(s.mnt_path("old_dir"), s.mnt_path("new_dir")).expect("rename dir");
        s.cli(&["checkpoint", "chk1"]).expect("checkpoint");

        fs::write(s.mnt_path("post.txt"), "post\n").expect("write post");

        s.cli(&["restore", "chk1"]).expect("restore");

        assert!(!s.mnt_path("old_dir").exists(), "old dir gone");
        assert!(s.mnt_path("new_dir").exists(), "new dir exists");
        assert_eq!(
            fs::read_to_string(s.mnt_path("new_dir/file.txt")).unwrap(),
            "content\n"
        );

        // Verify readdir reports d_type=dir for the renamed directory
        let entry = fs::read_dir(s.mnt_path(""))
            .expect("readdir")
            .filter_map(|e| e.ok())
            .find(|e| e.file_name() == "new_dir")
            .expect("new_dir in readdir");
        assert!(
            entry.file_type().expect("file_type").is_dir(),
            "renamed directory must have d_type=dir in readdir"
        );
    });
}

/// Renamed symlink has correct d_type (DT_LNK) after restore.
#[test]
fn restore_renamed_symlink_dtype() {
    let s = AgfsSession::new().expect("session setup");
    s.run_in_namespace(|| {
        fs::write(s.mnt_path("target.txt"), "target\n").expect("write target");
        std::os::unix::fs::symlink("target.txt", s.mnt_path("old_link")).expect("symlink");
        fs::rename(s.mnt_path("old_link"), s.mnt_path("new_link")).expect("rename symlink");
        s.cli(&["checkpoint", "chk1"]).expect("checkpoint");

        fs::write(s.mnt_path("post.txt"), "post\n").expect("write post");

        s.cli(&["restore", "chk1"]).expect("restore");

        assert!(!s.mnt_path("old_link").exists(), "old link gone");
        assert!(s.mnt_path("new_link").exists(), "new link exists");

        // Verify readdir reports d_type=symlink for the renamed symlink
        let entry = fs::read_dir(s.mnt_path(""))
            .expect("readdir")
            .filter_map(|e| e.ok())
            .find(|e| e.file_name() == "new_link")
            .expect("new_link in readdir");
        assert!(
            entry.file_type().expect("file_type").is_symlink(),
            "renamed symlink must have d_type=lnk in readdir"
        );

        // Content should be reachable through the symlink
        let content = fs::read_to_string(s.mnt_path("new_link")).expect("read through link");
        assert_eq!(content, "target\n");
    });
}

/// Writing after restore appends to the journal via the kernel's original
/// O_APPEND fd (which survived the set_len truncation without a reopen).
#[test]
fn kernel_appends_to_journal_after_restore() {
    let s = AgfsSession::new().expect("session setup");
    s.run_in_namespace(|| {
        fs::write(s.mnt_path("before.txt"), "before\n").expect("write before");
        s.cli(&["checkpoint", "chk1"]).expect("checkpoint");
        fs::write(s.mnt_path("discarded.txt"), "gone\n").expect("write discarded");

        s.cli(&["restore", "chk1"]).expect("restore");

        // This write goes through the kernel, which appends to the journal
        // using the same O_APPEND fd that was open before the truncation.
        fs::write(s.mnt_path("after.txt"), "after\n").expect("write after restore");

        let status = s.cli(&["status"]).expect("status");
        assert!(
            status.contains("after.txt"),
            "kernel journal append after restore should work: {status}"
        );
        assert!(
            !status.contains("discarded.txt"),
            "discarded file should not appear: {status}"
        );
    });
}

// ── Append-only journal / RST-record tests ─────────────────────────────

/// After restore, the journal is append-only (not truncated).
/// Verify that `agfs timeline` shows the restore event.
#[test]
fn timeline_shows_restore_event() {
    let s = AgfsSession::new().expect("session setup");
    s.run_in_namespace(|| {
        fs::write(s.mnt_path("a.txt"), "v1\n").expect("write");
        s.cli(&["checkpoint", "build"]).expect("checkpoint");

        fs::write(s.mnt_path("a.txt"), "v2\n").expect("write v2");
        s.cli(&["checkpoint", "test"]).expect("checkpoint");

        s.cli(&["restore", "build"]).expect("restore");

        let timeline = s.cli(&["timeline"]).expect("timeline");
        assert!(
            timeline.contains("restore"),
            "timeline should show restore event: {timeline}"
        );
        assert!(
            timeline.contains("build"),
            "timeline should reference target checkpoint: {timeline}"
        );
    });
}

/// Restore, make changes, checkpoint, then restore again (further back).
#[test]
fn multiple_restores_in_session() {
    let s = AgfsSession::new().expect("session setup");
    s.run_in_namespace(|| {
        fs::write(s.mnt_path("a.txt"), "v1\n").expect("write v1");
        s.cli(&["checkpoint", "c1"]).expect("checkpoint c1");

        fs::write(s.mnt_path("a.txt"), "v2\n").expect("write v2");
        s.cli(&["checkpoint", "c2"]).expect("checkpoint c2");

        // First restore: back to c1
        s.cli(&["restore", "c1"]).expect("restore to c1");

        fs::write(s.mnt_path("b.txt"), "new\n").expect("write b");
        s.cli(&["checkpoint", "c3"]).expect("checkpoint c3");

        // Second restore: back to c1 again (discards b.txt)
        s.cli(&["restore", "c1"]).expect("restore to c1 again");

        let status = s.cli(&["status"]).expect("status");
        assert!(
            !status.contains("b.txt"),
            "b.txt should be gone after second restore: {status}"
        );
    });
}

/// Restore then commit applies only the live changes.
#[test]
fn commit_after_multiple_restores() {
    let s = AgfsSession::new().expect("session setup");
    s.run_in_namespace(|| {
        fs::write(s.mnt_path("a.txt"), "v1\n").expect("write v1");
        s.cli(&["checkpoint", "c1"]).expect("checkpoint c1");

        fs::write(s.mnt_path("a.txt"), "v2\n").expect("write v2");
        s.cli(&["checkpoint", "c2"]).expect("checkpoint c2");

        s.cli(&["restore", "c1"]).expect("restore to c1");

        fs::write(s.mnt_path("extra.txt"), "extra\n").expect("write extra");

        s.cli(&["commit"]).expect("commit");

        // After commit, base should have a.txt=v1 and extra.txt
        let a_content = fs::read_to_string(s.base_path("a.txt")).expect("read a");
        assert_eq!(a_content, "v1\n", "a.txt should be v1 after commit");
        let extra_content = fs::read_to_string(s.base_path("extra.txt")).expect("read extra");
        assert_eq!(extra_content, "extra\n");
    });
}

/// Undo-restore: restore forward to a checkpoint that was in a dead zone.
#[test]
fn undo_restore() {
    let s = AgfsSession::new().expect("session setup");
    s.run_in_namespace(|| {
        fs::write(s.mnt_path("a.txt"), "v1\n").expect("write v1");
        s.cli(&["checkpoint", "c1"]).expect("checkpoint c1");

        fs::write(s.mnt_path("a.txt"), "v2\n").expect("write v2");
        fs::write(s.mnt_path("b.txt"), "new\n").expect("write b");
        s.cli(&["checkpoint", "c2"]).expect("checkpoint c2");

        // Restore back to c1 — kills the c1→c2 segment (v2 and b.txt).
        s.cli(&["restore", "c1"]).expect("restore to c1");
        let content = fs::read_to_string(s.mnt_path("a.txt")).expect("read a");
        assert_eq!(content, "v1\n", "should see v1 after first restore");
        assert!(!s.mnt_path("b.txt").exists(), "b.txt should be gone");

        // Undo the restore — restore forward to c2 (which is in the dead zone).
        s.cli(&["restore", "c2"]).expect("undo restore to c2");
        let content = fs::read_to_string(s.mnt_path("a.txt")).expect("read a");
        assert_eq!(content, "v2\n", "should see v2 after undo restore");
        assert!(s.mnt_path("b.txt").exists(), "b.txt should reappear");
    });
}

/// Create a new file (not in base), checkpoint, write to it again
/// (triggering re-COW), then restore to the checkpoint and commit.
/// The file should appear in base — it was staged at checkpoint time.
///
/// This exercises a bug where agfs_do_cow hardcodes overwrites=true,
/// flipping the flag for staged-only files.  If restore uses the
/// corrupted overwrites=true, the committed file might be missing or
/// the abort path might leave a ghost entry.
#[test]
fn restore_created_file_after_recow_then_commit() {
    let s = AgfsSession::new().expect("session setup");
    s.run_in_namespace(|| {
        // Create a brand-new file (not in base) and checkpoint.
        fs::write(s.mnt_path("newfile.txt"), "v1\n").expect("create");
        s.cli(&["checkpoint", "chk1"]).expect("checkpoint");

        // Write again — triggers re-COW (allocates new inode, may flip overwrites).
        fs::write(s.mnt_path("newfile.txt"), "v2\n").expect("write v2 (re-COW)");

        // Restore to chk1 — should see v1 content.
        s.cli(&["restore", "chk1"]).expect("restore");
        assert_eq!(
            fs::read_to_string(s.mnt_path("newfile.txt")).unwrap(),
            "v1\n",
            "mount should show checkpoint content after restore"
        );

        // Commit the restored state.
        s.cli(&["commit"]).expect("commit");

        // The file should appear in base with v1 content.
        assert_eq!(
            fs::read_to_string(s.base_path("newfile.txt")).unwrap(),
            "v1\n",
            "committed file should have checkpoint content in base"
        );
    });
}

/// Same as above, but abort instead of commit.  The created file
/// should NOT appear in base (it was never in base).
#[test]
fn restore_created_file_after_recow_then_abort() {
    let s = AgfsSession::new().expect("session setup");
    s.run_in_namespace(|| {
        fs::write(s.mnt_path("newfile.txt"), "v1\n").expect("create");
        s.cli(&["checkpoint", "chk1"]).expect("checkpoint");
        fs::write(s.mnt_path("newfile.txt"), "v2\n").expect("write v2 (re-COW)");

        s.cli(&["restore", "chk1"]).expect("restore");
        s.cli(&["abort"]).expect("abort");

        assert!(
            !s.base_path("newfile.txt").exists(),
            "staged-only file should not appear in base after abort"
        );
    });
}

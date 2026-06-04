use crate::helpers::YoloSession;
use std::fs;

#[test]
fn diff_empty() {
    let s = YoloSession::new().expect("session setup");

    let output = s.cli(&["review", "--diff"]).expect("diff");
    assert!(output.contains("No changes staged"), "output: {output}");
}

#[test]
fn diff_modified_file() {
    let s = YoloSession::new().expect("session setup");

    fs::write(s.mnt_path("hello.txt"), "new content\n").unwrap();

    let output = s.cli(&["review", "--diff"]).expect("diff");
    assert!(output.contains("hello.txt"), "output: {output}");
    assert!(output.contains("modified"), "output: {output}");
    assert!(output.contains("+new content"), "output: {output}");
}

#[test]
fn diff_new_file() {
    let s = YoloSession::new().expect("session setup");

    fs::write(s.mnt_path("added.txt"), "brand new\n").unwrap();

    let output = s.cli(&["review", "--diff"]).expect("diff");
    assert!(output.contains("added.txt"), "output: {output}");
    assert!(output.contains("+brand new"), "output: {output}");
}

#[test]
fn diff_deleted_file() {
    let s = YoloSession::new().expect("session setup");

    fs::remove_file(s.mnt_path("hello.txt")).unwrap();

    let output = s.cli(&["review", "--diff"]).expect("diff");
    assert!(output.contains("hello.txt"), "output: {output}");
    assert!(
        output.contains("deleted"),
        "diff should indicate deletion: {output}"
    );
}

#[test]
fn diff_renamed_file() {
    let s = YoloSession::new().expect("session setup");

    fs::rename(s.mnt_path("hello.txt"), s.mnt_path("moved.txt")).unwrap();

    let output = s.cli(&["review", "--diff"]).expect("diff");
    assert!(
        output.contains("hello.txt"),
        "diff should mention old name: {output}"
    );
    assert!(
        output.contains("moved.txt"),
        "diff should mention new name: {output}"
    );
}

#[test]
fn diff_single_file_shows_only_that_file() {
    let s = YoloSession::new().expect("session setup");

    fs::write(s.mnt_path("hello.txt"), "changed\n").unwrap();
    fs::write(s.mnt_path("other.txt"), "also changed\n").unwrap();

    let output = s.cli(&["review", "--diff", "--", "hello.txt"]).expect("diff -- hello.txt");
    assert!(
        output.contains("hello.txt"),
        "should show hello.txt: {output}"
    );
    assert!(
        !output.contains("other.txt"),
        "should NOT show other.txt: {output}"
    );
}

#[test]
fn diff_single_file_not_changed() {
    let s = YoloSession::new().expect("session setup");

    fs::write(s.mnt_path("hello.txt"), "changed\n").unwrap();

    let output = s.cli(&["review", "--diff", "--", "other.txt"]).expect("diff -- other.txt");
    assert!(
        output.contains("No changes staged"),
        "no matching changes: {output}"
    );
}

#[test]
fn diff_single_file_with_absolute_path() {
    let s = YoloSession::new().expect("session setup");

    fs::write(s.mnt_path("hello.txt"), "changed\n").unwrap();

    let abs = format!("{}/hello.txt", s.root.display());
    let output = s.cli(&["review", "--diff", "--", &abs]).expect("diff -- absolute path");
    assert!(
        output.contains("hello.txt"),
        "should find with absolute path: {output}"
    );
}

/// Creating a staged-only file, then deleting it, should produce no diff
/// output (the tombstone is spurious — nothing in base to hide).
#[test]
fn diff_spurious_tombstone_skipped() {
    let s = YoloSession::new().expect("session setup");

    fs::write(s.mnt_path("ephemeral.txt"), "temporary\n").unwrap();
    fs::remove_file(s.mnt_path("ephemeral.txt")).unwrap();

    let output = s.cli(&["review", "--diff"]).expect("diff");
    assert!(
        !output.contains("ephemeral.txt"),
        "spurious tombstone should not appear in diff: {output}"
    );
}

/// Verify that diff correctly derives "added" vs "modified" from the base
/// filesystem: a new file shows as added, an overwritten base file shows
/// as modified.
#[test]
fn diff_add_vs_modify_classification() {
    let s = YoloSession::new().expect("session setup");

    // hello.txt exists in base → overwrite is a modification
    fs::write(s.mnt_path("hello.txt"), "changed\n").unwrap();
    // brand_new.txt does not exist in base → creation is an addition
    fs::write(s.mnt_path("brand_new.txt"), "fresh\n").unwrap();

    let output = s.cli(&["review", "--diff"]).expect("diff");
    assert!(
        output.contains("hello.txt") && output.contains("modified"),
        "base file overwrite should show as modified: {output}"
    );
    assert!(
        output.contains("brand_new.txt") && output.contains("added"),
        "new file should show as added: {output}"
    );
}

/// After travel, diff should only show snapshot-state changes,
/// not post-snapshot mutations (which are in the dead zone).
#[test]
fn diff_after_travel_excludes_dead_zone() {
    let s = YoloSession::new().expect("session setup");

    fs::write(s.mnt_path("hello.txt"), "wanted\n").unwrap();
    s.cli(&["snapshot", "chk1"]).expect("snapshot");

    // Create a new file after snapshot (becomes dead zone)
    fs::write(s.mnt_path("post_chk.txt"), "dead\n").unwrap();

    s.cli(&["travel", "chk1"]).expect("travel");

    let output = s.cli(&["review", "--diff"]).expect("diff");
    assert!(
        output.contains("hello.txt"),
        "snapshot change should appear in diff: {output}"
    );
    assert!(
        !output.contains("post_chk.txt"),
        "dead-zone file should NOT appear in diff: {output}"
    );
}

/// Diff should indicate binary files instead of showing garbled content.
#[test]
fn diff_new_binary_file() {
    let s = YoloSession::new().expect("session setup");

    // Write a file with non-UTF-8 content (null bytes, high bytes).
    let binary_data: Vec<u8> = (0..256).map(|i| i as u8).collect();
    fs::write(s.mnt_path("image.bin"), &binary_data).unwrap();

    let output = s.cli(&["review", "--diff"]).expect("diff");
    assert!(
        output.contains("image.bin"),
        "binary file should appear in diff: {output}"
    );
    assert!(
        output.contains("Binary") || output.contains("binary"),
        "diff should indicate binary file: {output}"
    );
}

/// Diff of a modified binary base file should indicate binary change.
#[test]
fn diff_modified_binary_file() {
    let s = YoloSession::new().expect("session setup");

    // test.sh exists in base. Overwrite with binary content.
    let binary_data = vec![0u8, 1, 2, 0xFF, 0xFE, 0, 0, 3];
    fs::write(s.mnt_path("test.sh"), &binary_data).unwrap();

    let output = s.cli(&["review", "--diff"]).expect("diff");
    assert!(
        output.contains("test.sh"),
        "modified binary file should appear: {output}"
    );
    assert!(
        output.contains("Binary") || output.contains("binary"),
        "diff should indicate binary content: {output}"
    );
}

/// Diff between two snapshots that span a travel should still work.
#[test]
fn diff_between_snapshots_spanning_travel() {
    let s = YoloSession::new().expect("session setup");

    fs::write(s.mnt_path("hello.txt"), "v1\n").unwrap();
    s.cli(&["snapshot", "chk1"]).expect("snapshot 1");

    fs::write(s.mnt_path("hello.txt"), "v2\n").unwrap();
    fs::write(s.mnt_path("extra.txt"), "extra\n").unwrap();
    s.cli(&["snapshot", "chk2"]).expect("snapshot 2");

    // Travel to chk1, then work and snapshot again
    s.cli(&["travel", "chk1"]).expect("travel");
    s.cli(&["snapshot", "post-travel"])
        .expect("snapshot post-travel");
    fs::write(s.mnt_path("hello.txt"), "v3\n").unwrap();
    s.cli(&["snapshot", "chk3"]).expect("snapshot 3");

    // Diff from chk1 to chk3 should NOT include chk2's dead-zone changes.
    // Gen ids: chk1=1, chk2=2, travel=3, post-travel=4, chk3=5 → range 1..5.
    // (chk2's segment falls inside that range by index, but the liveness mask
    // drops it as a dead zone, so extra.txt never appears.)
    let output = s.cli(&["review", "--diff", "1..5"]).expect("diff 1..5");
    assert!(
        !output.contains("extra.txt"),
        "dead-zone file should NOT appear: {output}"
    );
}

/// A file modified under a RENAMED base directory is classified vs its base
/// backing (through the rename redirect), so it's "modified" with a minimal
/// diff — not "added" with the whole file. Regression for the literal-path
/// base check that ignored ancestor BasePath redirects.
#[test]
fn modify_child_of_renamed_base_dir_is_modified() {
    let s = YoloSession::new().expect("session setup");

    // Rename a base dir, snapshot it, then modify a child (backed by the base
    // subdir/deep.txt via the rename redirect).
    fs::rename(s.mnt_path("subdir"), s.mnt_path("moved")).expect("rename dir");
    s.cli(&["snapshot", "c1"]).expect("snapshot");
    fs::write(s.mnt_path("moved/deep.txt"), "nested\nextra\n").expect("modify child");

    let diff = s.cli(&["review", "--diff"]).expect("diff");
    assert!(
        diff.contains("deep.txt") && diff.contains("modified"),
        "child of renamed dir should be modified: {diff}"
    );
    assert!(
        !diff.contains("added"),
        "should not be classified as added: {diff}"
    );
    assert!(
        diff.contains("+extra"),
        "diff should be just the appended line: {diff}"
    );
}

/// `diff` of a deleted file shows its removed content (read from the delete's
/// pre-image), not just a "(deleted)" header.
#[test]
fn diff_deleted_file_shows_removed_content() {
    let s = YoloSession::new().expect("session setup");

    fs::remove_file(s.mnt_path("hello.txt")).expect("delete"); // base = "base content\n"

    let diff = s.cli(&["review", "--diff"]).expect("diff");
    assert!(diff.contains("deleted"), "should show deleted: {diff}");
    assert!(
        diff.contains("-base content"),
        "should show the removed content from the pre-image: {diff}"
    );
}

/// `diff` after a re-COW (modify across a snapshot) reads the old content from
/// the prior staged inode (the pre-image), showing v1 → v2.
#[test]
fn diff_recow_after_snapshot_shows_prior_content() {
    let s = YoloSession::new().expect("session setup");

    fs::write(s.mnt_path("created.txt"), "v1\n").expect("create");
    s.cli(&["snapshot", "c1"]).expect("snapshot");
    fs::write(s.mnt_path("created.txt"), "v2\n").expect("modify");

    let diff = s.cli(&["review", "--diff"]).expect("diff");
    assert!(
        diff.contains("-v1") && diff.contains("+v2"),
        "re-COW diff should show v1 → v2 from the prior staged inode: {diff}"
    );
}

/// `diff --each` shows one unified-diff stanza per consecutive snapshot, each
/// under a `snapshot [id]` header, with that snapshot's own content.
#[test]
fn diff_each_shows_per_snapshot_stanzas() {
    let s = YoloSession::new().expect("session setup");

    fs::write(s.mnt_path("hello.txt"), "v1\n").expect("modify"); // base = "base content\n"
    s.cli(&["snapshot", "c1"]).expect("snapshot c1"); // gen 1
    fs::write(s.mnt_path("added.txt"), "fresh\n").expect("create");
    s.cli(&["snapshot", "c2"]).expect("snapshot c2"); // gen 2

    let diff = s.cli(&["review", "--diff", "--each"]).expect("diff --each");
    // Step 1 (snapshot 1): hello.txt modified base content → v1.
    assert!(
        diff.contains("snapshot [1]") && diff.contains("+v1"),
        "step 1 should show snapshot [1] with hello.txt's diff: {diff}"
    );
    // Step 2 (snapshot 2): added.txt added.
    assert!(
        diff.contains("snapshot [2]") && diff.contains("+fresh"),
        "step 2 should show snapshot [2] with added.txt: {diff}"
    );
}

/// `diff 0..` uses each path's FIRST-touch pre-image (the base), not the latest
/// intermediate version — so it diffs base → final, never against v1.
#[test]
fn diff_base_uses_base_not_intermediate() {
    let s = YoloSession::new().expect("session setup");

    fs::write(s.mnt_path("hello.txt"), "v1\n").expect("modify 1"); // base = "base content\n"
    s.cli(&["snapshot", "c1"]).expect("snapshot");
    fs::write(s.mnt_path("hello.txt"), "v2\n").expect("modify 2");

    let diff = s.cli(&["review", "--diff", "0.."]).expect("diff 0..");
    assert!(
        diff.contains("-base content"),
        "0.. old side should be the base (first touch), not v1: {diff}"
    );
    assert!(diff.contains("+v2"), "0.. new side should be v2: {diff}");
    assert!(
        !diff.contains("v1"),
        "0.. must not diff against the intermediate v1: {diff}"
    );
}

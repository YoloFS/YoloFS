use crate::helpers::YoloSession;
use std::fs;

#[test]
fn status_empty() {
    let s = YoloSession::new().expect("session setup");

    let output = s.cli(&["review"]).expect("status");
    assert!(output.contains("No changes staged"), "output: {output}");
}

#[test]
fn status_modified() {
    let s = YoloSession::new().expect("session setup");

    fs::write(s.mnt_path("hello.txt"), "changed\n").unwrap();

    let output = s.cli(&["review"]).expect("status");
    assert!(output.contains("modified"), "output: {output}");
    assert!(output.contains("hello.txt"), "output: {output}");
    assert!(output.contains("1 staged change"), "output: {output}");
}

#[test]
fn status_multiple_changes() {
    let s = YoloSession::new().expect("session setup");

    fs::write(s.mnt_path("hello.txt"), "changed\n").unwrap();
    fs::write(s.mnt_path("newfile.txt"), "new\n").unwrap();

    let output = s.cli(&["review"]).expect("status");
    assert!(output.contains("hello.txt"), "output: {output}");
    assert!(output.contains("newfile.txt"), "output: {output}");
    assert!(output.contains("2 staged change"), "output: {output}");
}

#[test]
fn status_deleted() {
    let s = YoloSession::new().expect("session setup");

    fs::remove_file(s.mnt_path("hello.txt")).unwrap();

    let output = s.cli(&["review"]).expect("status");
    assert!(
        output.contains("deleted"),
        "status should show deleted: {output}"
    );
    assert!(output.contains("hello.txt"), "output: {output}");
    assert!(output.contains("1 staged change"), "output: {output}");
}

#[test]
fn status_renamed() {
    let s = YoloSession::new().expect("session setup");

    fs::rename(s.mnt_path("hello.txt"), s.mnt_path("moved.txt")).unwrap();

    let output = s.cli(&["review"]).expect("status");
    assert!(
        output.contains("renamed"),
        "status should show renamed: {output}"
    );
    assert!(output.contains("moved.txt"), "output: {output}");
}

/// After travel, status should only show snapshot-state changes,
/// not post-snapshot mutations (which are in the dead zone).
#[test]
fn status_after_travel_excludes_dead_zone() {
    let s = YoloSession::new().expect("session setup");

    // Modify before snapshot
    fs::write(s.mnt_path("hello.txt"), "wanted\n").unwrap();
    s.cli(&["snapshot", "chk1"]).expect("snapshot");

    // Create a new file after snapshot (dead zone after travel)
    fs::write(s.mnt_path("post_chk.txt"), "dead\n").unwrap();

    s.cli(&["travel", "chk1"]).expect("travel");

    let output = s.cli(&["review"]).expect("status");
    assert!(
        output.contains("hello.txt"),
        "snapshot change should appear: {output}"
    );
    assert!(
        !output.contains("post_chk.txt"),
        "dead-zone file should NOT appear in status: {output}"
    );
}

/// `status <id>` targeting a snapshot before a travel shows only that
/// snapshot's own change. (`chk1` is the first snapshot → gen id 1.)
#[test]
fn status_at_snapshot_after_travel() {
    let s = YoloSession::new().expect("session setup");

    fs::write(s.mnt_path("hello.txt"), "v1\n").unwrap();
    s.cli(&["snapshot", "chk1"]).expect("snapshot 1");

    fs::write(s.mnt_path("extra.txt"), "extra\n").unwrap();
    s.cli(&["snapshot", "chk2"]).expect("snapshot 2");

    s.cli(&["travel", "chk1"]).expect("travel");

    let output = s.cli(&["review", "1"]).expect("status 1");
    assert!(
        output.contains("hello.txt"),
        "snapshot 1's change should appear: {output}"
    );
    assert!(
        !output.contains("extra.txt"),
        "chk2-only change should NOT appear: {output}"
    );
}

/// `yolo review` defaults to the latest snapshot's changes; `0..` shows
/// everything vs base, and the default view hints at it.
#[test]
fn status_defaults_to_latest_snapshot() {
    let s = YoloSession::new().expect("session setup");

    fs::write(s.mnt_path("a.txt"), "a\n").expect("write a");
    s.cli(&["snapshot", "chk1"]).expect("snapshot chk1");
    fs::write(s.mnt_path("b.txt"), "b\n").expect("write b");
    s.cli(&["snapshot", "chk2"]).expect("snapshot chk2");

    let latest = s.cli(&["review"]).expect("status");
    assert!(
        latest.contains("b.txt"),
        "latest should show b.txt: {latest}"
    );
    assert!(
        !latest.contains("a.txt"),
        "latest should NOT show the older a.txt: {latest}"
    );
    assert!(
        latest.contains("review all"),
        "default view should hint at the vs-base view (`yolo review all`): {latest}"
    );

    // `all` is the readable name for everything vs base — shows both snapshots.
    let full = s.cli(&["review", "all"]).expect("review all");
    assert!(full.contains("a.txt"), "all should show a.txt: {full}");
    assert!(full.contains("b.txt"), "all should show b.txt: {full}");
}

/// The default view diffs vs the PREVIOUS snapshot: a staged-only file deleted
/// in a later snapshot shows as "deleted". But `0..` diffs vs the base, where
/// that file never existed, so it nets to nothing.
#[test]
fn delete_of_staged_file_shows_vs_prev_but_not_full() {
    let s = YoloSession::new().expect("session setup");

    // Add a brand-new file (absent from base) and snapshot it.
    fs::write(s.mnt_path("staged_only.txt"), "hi\n").expect("write");
    s.cli(&["snapshot", "c1"]).expect("snapshot c1");

    // Delete it, then snapshot so it sits in its own segment.
    fs::remove_file(s.mnt_path("staged_only.txt")).expect("rm");
    s.cli(&["snapshot", "c2"]).expect("snapshot c2");

    // Default status is vs the previous snapshot (c1, where it existed).
    let status = s.cli(&["review"]).expect("status");
    assert!(
        status.contains("staged_only.txt") && status.contains("deleted"),
        "default status should show the file deleted vs prev snapshot: {status}"
    );

    // `0..` is vs base: the file never existed there, so nothing nets out.
    let full = s.cli(&["review", "0.."]).expect("status 0..");
    assert!(
        !full.contains("staged_only.txt"),
        "0.. (vs base) should show nothing for a staged-only add+delete: {full}"
    );
}

/// A file modified under a RENAMED base directory shows as "modified", not
/// "added": its stage records a pre-image pointing at the real backing
/// (subdir/deep.txt) — the rename redirect is resolved at copy-up. Mirrors the
/// `diff` regression `modify_child_of_renamed_base_dir_is_modified`, but
/// exercises the O(segment) vs-previous-snapshot status path.
#[test]
fn modify_child_of_renamed_dir_shows_modified() {
    let s = YoloSession::new().expect("session setup");

    // Rename a base dir, snapshot, then modify a child (backed via the redirect).
    fs::rename(s.mnt_path("subdir"), s.mnt_path("moved")).expect("rename dir");
    s.cli(&["snapshot", "c1"]).expect("snapshot");
    fs::write(s.mnt_path("moved/deep.txt"), "nested\nextra\n").expect("modify child");

    let output = s.cli(&["review"]).expect("status");
    assert!(
        output.contains("deep.txt") && output.contains("modified"),
        "child of renamed dir should be modified, not added: {output}"
    );
    assert!(
        !output.contains("(added)"),
        "should not be classified as added: {output}"
    );
}

/// The pre-image is relative to the PREVIOUS SNAPSHOT, not the base: a file
/// created this session, snapshotted, then modified re-COWs with a pre-image
/// (the prior staged inode), so the default (vs-prev) status shows "modified"
/// — not "added".
#[test]
fn recow_after_snapshot_shows_modified_vs_prev() {
    let s = YoloSession::new().expect("session setup");

    // Create a brand-new file (absent from base) and snapshot it.
    fs::write(s.mnt_path("created.txt"), "v1\n").expect("create");
    s.cli(&["snapshot", "c1"]).expect("snapshot");

    // Modify it after the snapshot: the re-COW records a pre-image (prior inode).
    fs::write(s.mnt_path("created.txt"), "v2\n").expect("modify");

    let output = s.cli(&["review"]).expect("status");
    assert!(
        output.contains("created.txt") && output.contains("modified"),
        "re-COW after snapshot is modified vs the previous snapshot: {output}"
    );
}

/// A rename renders as a single `(renamed)` entry — the vacated source is not
/// also listed as `(deleted)` (the changeset drops that tombstone).
#[test]
fn rename_shows_only_renamed_not_deleted() {
    let s = YoloSession::new().expect("session setup");

    fs::rename(s.mnt_path("hello.txt"), s.mnt_path("moved.txt")).expect("rename");

    let output = s.cli(&["review"]).expect("status");
    assert!(
        output.contains("renamed") && output.contains("moved.txt"),
        "should show the rename: {output}"
    );
    assert!(
        !output.contains("deleted"),
        "rename must not also show a deleted line for the vacated source: {output}"
    );
}

/// `status --each` expands the whole session into one summary per consecutive
/// snapshot, each under a `snapshot <id>` header (gen id == marker index).
#[test]
fn status_each_shows_one_summary_per_snapshot() {
    let s = YoloSession::new().expect("session setup");

    fs::write(s.mnt_path("a.txt"), "a\n").expect("write a");
    s.cli(&["snapshot", "c1"]).expect("snapshot c1"); // gen 1
    fs::write(s.mnt_path("b.txt"), "b\n").expect("write b");
    s.cli(&["snapshot", "c2"]).expect("snapshot c2"); // gen 2

    let output = s.cli(&["review", "--each"]).expect("status --each");
    assert!(
        output.contains("snapshot 1") && output.contains("a.txt"),
        "step 1 should head snapshot 1 with a.txt: {output}"
    );
    assert!(
        output.contains("snapshot 2") && output.contains("b.txt"),
        "step 2 should head snapshot 2 with b.txt: {output}"
    );
}

/// `--each` labels the tip (work after the last snapshot, not snapshotted) as
/// `working`, not a phantom `snapshot N` — that snapshot doesn't exist yet.
#[test]
fn each_labels_working_tip() {
    let s = YoloSession::new().expect("session setup");

    fs::write(s.mnt_path("a.txt"), "a\n").expect("write a");
    s.cli(&["snapshot", "c1"]).expect("snapshot c1"); // gen 1
    fs::write(s.mnt_path("b.txt"), "b\n").expect("write b"); // not snapshotted

    let output = s.cli(&["review", "--each"]).expect("status --each");
    assert!(
        output.contains("snapshot 1") && output.contains("a.txt"),
        "snapshot 1's change should still be headed `snapshot 1`: {output}"
    );
    assert!(
        output.contains("working") && output.contains("b.txt"),
        "the tip should be headed `working` with b.txt: {output}"
    );
    assert!(
        !output.contains("snapshot 2"),
        "must not invent a `snapshot 2` that doesn't exist: {output}"
    );
}

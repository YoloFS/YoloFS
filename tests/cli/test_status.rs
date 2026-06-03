use crate::helpers::YoloSession;
use std::fs;

#[test]
fn status_empty() {
    let s = YoloSession::new().expect("session setup");

    let output = s.cli(&["status"]).expect("status");
    assert!(output.contains("No changes staged"), "output: {output}");
}

#[test]
fn status_modified() {
    let s = YoloSession::new().expect("session setup");

    fs::write(s.mnt_path("hello.txt"), "changed\n").unwrap();

    let output = s.cli(&["status"]).expect("status");
    assert!(output.contains("modified"), "output: {output}");
    assert!(output.contains("hello.txt"), "output: {output}");
    assert!(output.contains("1 staged change"), "output: {output}");
}

#[test]
fn status_multiple_changes() {
    let s = YoloSession::new().expect("session setup");

    fs::write(s.mnt_path("hello.txt"), "changed\n").unwrap();
    fs::write(s.mnt_path("newfile.txt"), "new\n").unwrap();

    let output = s.cli(&["status"]).expect("status");
    assert!(output.contains("hello.txt"), "output: {output}");
    assert!(output.contains("newfile.txt"), "output: {output}");
    assert!(output.contains("2 staged change"), "output: {output}");
}

#[test]
fn status_deleted() {
    let s = YoloSession::new().expect("session setup");

    fs::remove_file(s.mnt_path("hello.txt")).unwrap();

    let output = s.cli(&["status"]).expect("status");
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

    let output = s.cli(&["status"]).expect("status");
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

    let output = s.cli(&["status"]).expect("status");
    assert!(
        output.contains("hello.txt"),
        "snapshot change should appear: {output}"
    );
    assert!(
        !output.contains("post_chk.txt"),
        "dead-zone file should NOT appear in status: {output}"
    );
}

/// Status with --at targeting a snapshot before a travel
/// should show only changes at that snapshot.
#[test]
fn status_at_snapshot_after_travel() {
    let s = YoloSession::new().expect("session setup");

    fs::write(s.mnt_path("hello.txt"), "v1\n").unwrap();
    s.cli(&["snapshot", "chk1"]).expect("snapshot 1");

    fs::write(s.mnt_path("extra.txt"), "extra\n").unwrap();
    s.cli(&["snapshot", "chk2"]).expect("snapshot 2");

    s.cli(&["travel", "chk1"]).expect("travel");

    let output = s.cli(&["status", "--at", "chk1"]).expect("status --at");
    assert!(
        output.contains("hello.txt"),
        "chk1 change should appear: {output}"
    );
    assert!(
        !output.contains("extra.txt"),
        "chk2-only change should NOT appear: {output}"
    );
}

/// `yolo status` defaults to the latest snapshot's changes; `--full` shows all,
/// and the scoped view hints about `--full`.
#[test]
fn status_defaults_to_latest_snapshot() {
    let s = YoloSession::new().expect("session setup");

    fs::write(s.mnt_path("a.txt"), "a\n").expect("write a");
    s.cli(&["snapshot", "chk1"]).expect("snapshot chk1");
    fs::write(s.mnt_path("b.txt"), "b\n").expect("write b");
    s.cli(&["snapshot", "chk2"]).expect("snapshot chk2");

    let latest = s.cli(&["status"]).expect("status");
    assert!(
        latest.contains("b.txt"),
        "latest should show b.txt: {latest}"
    );
    assert!(
        !latest.contains("a.txt"),
        "latest should NOT show the older a.txt: {latest}"
    );
    assert!(
        latest.contains("--full"),
        "scoped view should hint about --full: {latest}"
    );

    let full = s.cli(&["status", "--full"]).expect("status --full");
    assert!(full.contains("a.txt"), "--full should show a.txt: {full}");
    assert!(full.contains("b.txt"), "--full should show b.txt: {full}");
}

/// The default view diffs vs the PREVIOUS snapshot: a staged-only file deleted
/// in a later snapshot shows as "deleted". But `--full` diffs vs the base,
/// where that file never existed, so it nets to nothing.
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
    let status = s.cli(&["status"]).expect("status");
    assert!(
        status.contains("staged_only.txt") && status.contains("deleted"),
        "default status should show the file deleted vs prev snapshot: {status}"
    );

    // --full is vs base: the file never existed there, so nothing nets out.
    let full = s.cli(&["status", "--full"]).expect("status --full");
    assert!(
        !full.contains("staged_only.txt"),
        "--full (vs base) should show nothing for a staged-only add+delete: {full}"
    );
}

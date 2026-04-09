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

/// After restore, status should only show checkpoint-state changes,
/// not post-checkpoint mutations (which are in the dead zone).
#[test]
fn status_after_restore_excludes_dead_zone() {
    let s = YoloSession::new().expect("session setup");

    // Modify before checkpoint
    fs::write(s.mnt_path("hello.txt"), "wanted\n").unwrap();
    s.cli(&["checkpoint", "chk1"]).expect("checkpoint");

    // Create a new file after checkpoint (dead zone after restore)
    fs::write(s.mnt_path("post_chk.txt"), "dead\n").unwrap();

    s.cli(&["restore", "chk1"]).expect("restore");

    let output = s.cli(&["status"]).expect("status");
    assert!(
        output.contains("hello.txt"),
        "checkpoint change should appear: {output}"
    );
    assert!(
        !output.contains("post_chk.txt"),
        "dead-zone file should NOT appear in status: {output}"
    );
}

/// Status with --at targeting a checkpoint before a restore
/// should show only changes at that checkpoint.
#[test]
fn status_at_checkpoint_after_restore() {
    let s = YoloSession::new().expect("session setup");

    fs::write(s.mnt_path("hello.txt"), "v1\n").unwrap();
    s.cli(&["checkpoint", "chk1"]).expect("checkpoint 1");

    fs::write(s.mnt_path("extra.txt"), "extra\n").unwrap();
    s.cli(&["checkpoint", "chk2"]).expect("checkpoint 2");

    s.cli(&["restore", "chk1"]).expect("restore");

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

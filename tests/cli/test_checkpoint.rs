use crate::helpers::AgfsSession;
use std::fs;

/// Creating a checkpoint is visible via `agfs log`.
#[test]
fn checkpoint_visible_in_log() {
    let s = AgfsSession::new().expect("session setup");

    fs::write(s.mnt_path("hello.txt"), "modified\n").expect("write");
    s.cli(&["checkpoint", "build"]).expect("checkpoint");

    let log = s.cli(&["log"]).expect("log");
    assert!(
        log.contains("build"),
        "agfs log should list the 'build' checkpoint: {log}"
    );
}

/// Checkpoint list shows created checkpoints.
#[test]
fn checkpoint_list() {
    let s = AgfsSession::new().expect("session setup");

    fs::write(s.mnt_path("hello.txt"), "v1\n").expect("write");
    s.cli(&["checkpoint", "first"]).expect("checkpoint 1");

    fs::write(s.mnt_path("hello.txt"), "v2\n").expect("write");
    s.cli(&["checkpoint", "second"]).expect("checkpoint 2");

    let output = s.cli(&["log"]).expect("checkpoint list");
    assert!(output.contains("first"), "should list first: {output}");
    assert!(output.contains("second"), "should list second: {output}");
}

/// Status --at shows state at a checkpoint, not the current state.
#[test]
fn status_at_checkpoint() {
    let s = AgfsSession::new().expect("session setup");

    // Modify hello.txt, checkpoint, then create another file
    fs::write(s.mnt_path("hello.txt"), "modified\n").expect("write");
    s.cli(&["checkpoint", "checkpoint"]).expect("checkpoint");

    // Create a new file after the checkpoint
    fs::write(s.mnt_path("new_after_chk.txt"), "new content\n").expect("write new");

    // Status --at checkpoint should not show new_after_chk.txt
    let at_output = s
        .cli(&["status", "--at", "checkpoint"])
        .expect("status --at");
    assert!(
        at_output.contains("hello.txt"),
        "should show hello.txt: {at_output}"
    );
    assert!(
        !at_output.contains("new_after_chk.txt"),
        "should NOT show new_after_chk.txt: {at_output}"
    );

    // Regular status should show both
    let full_output = s.cli(&["status"]).expect("status");
    assert!(
        full_output.contains("hello.txt"),
        "full should show hello.txt: {full_output}"
    );
    assert!(
        full_output.contains("new_after_chk.txt"),
        "full should show new_after_chk.txt: {full_output}"
    );
}

/// Re-COW: writing after a checkpoint preserves the old inode.
#[test]
fn recow_preserves_checkpoint_inode() {
    let s = AgfsSession::new().expect("session setup");

    // Write v1
    fs::write(s.mnt_path("hello.txt"), "version1\n").expect("write v1");
    s.cli(&["checkpoint", "v1"]).expect("checkpoint v1");

    // Write v2 (should trigger re-COW)
    fs::write(s.mnt_path("hello.txt"), "version2\n").expect("write v2");

    // Current content should be v2
    let current = fs::read_to_string(s.mnt_path("hello.txt")).expect("read current");
    assert_eq!(current, "version2\n");

    // Re-COW: status --at v1 should show the checkpoint state, proving inode preserved
    let at_v1 = s.cli(&["status", "--at", "v1"]).expect("status --at v1");
    assert!(
        at_v1.contains("hello.txt"),
        "should show hello.txt at v1: {at_v1}"
    );
    assert!(
        at_v1.contains("1 staged change"),
        "v1 checkpoint should have exactly 1 change: {at_v1}"
    );
}

/// Restore to a checkpoint reverts the mount to the checkpoint state.
#[test]
fn restore_to_checkpoint() {
    let s = AgfsSession::new().expect("session setup");

    // Write a file, checkpoint, write another
    fs::write(s.mnt_path("hello.txt"), "committed version\n").expect("write hello");
    s.cli(&["checkpoint", "checkpoint"]).expect("checkpoint");

    // Create a new file after checkpoint
    fs::write(s.mnt_path("new_after.txt"), "staged only\n").expect("write new");

    // Restore to the checkpoint
    s.cli(&["restore", "checkpoint"]).expect("restore");

    // hello.txt should still be staged (visible through mount)
    let content = fs::read_to_string(s.mnt_path("hello.txt")).expect("read through mount");
    assert_eq!(content, "committed version\n");

    // new_after.txt should no longer be visible through the mount
    assert!(
        !s.mnt_path("new_after.txt").exists(),
        "post-checkpoint file should not be visible after restore"
    );
}

/// Checkpoint with no name uses a timestamp.
#[test]
fn checkpoint_default_name() {
    let s = AgfsSession::new().expect("session setup");
    fs::write(s.mnt_path("hello.txt"), "modified\n").expect("write");

    s.cli(&["checkpoint"]).expect("checkpoint with no name");

    let output = s.cli(&["log"]).expect("list");
    assert!(
        output.contains("chk-"),
        "default name should start with 'chk-': {output}"
    );
}

/// Two handles writing the same file: second handle sees the first handle's writes.
#[test]
fn two_handles_same_file_no_checkpoint() {
    let s = AgfsSession::new().expect("session setup");

    // Handle A writes v1
    fs::write(s.mnt_path("hello.txt"), "handleA\n").expect("write A");

    // Handle B opens the same file and writes v2
    fs::write(s.mnt_path("hello.txt"), "handleB\n").expect("write B");

    // Current content is B's version
    let content = fs::read_to_string(s.mnt_path("hello.txt")).expect("read");
    assert_eq!(content, "handleB\n");

    // Base unchanged
    let base = fs::read_to_string(s.base_path("hello.txt")).expect("read base");
    assert_eq!(base, "base content\n");

    // Only one staged inode needed (no checkpoint → no re-COW)
    let status = s.cli(&["status"]).expect("status");
    assert!(
        status.contains("hello.txt"),
        "status should show hello.txt: {status}"
    );
    assert!(
        status.contains("1 staged change"),
        "should be exactly 1 change: {status}"
    );
}

/// After a checkpoint, a second handle's write triggers re-COW,
/// preserving the checkpoint inode while both handles produce correct reads.
#[test]
fn two_handles_recow_after_checkpoint() {
    let s = AgfsSession::new().expect("session setup");

    // Handle A writes v1
    fs::write(s.mnt_path("hello.txt"), "v1\n").expect("write v1");

    // Checkpoint preserves v1 state
    s.cli(&["checkpoint", "s1"]).expect("checkpoint");

    // Handle B writes v2 (triggers re-COW: v1 inode preserved, new inode for v2)
    fs::write(s.mnt_path("hello.txt"), "v2\n").expect("write v2");

    // Current content is v2
    let content = fs::read_to_string(s.mnt_path("hello.txt")).expect("read");
    assert_eq!(content, "v2\n");

    // status --at s1 should show the v1 checkpoint state (proves re-COW preserved it)
    let at_s1 = s.cli(&["status", "--at", "s1"]).expect("status --at s1");
    assert!(
        at_s1.contains("hello.txt"),
        "checkpoint state should have hello.txt: {at_s1}"
    );
    assert!(
        at_s1.contains("1 staged change"),
        "checkpoint s1 should have exactly 1 change: {at_s1}"
    );

    // Base still original
    let base = fs::read_to_string(s.base_path("hello.txt")).expect("read base");
    assert_eq!(base, "base content\n");
}

/// Second handle opening after another handle already re-COW'd should NOT
/// trigger a redundant re-COW (per-inode cow_checkpoint_gen optimization).
#[test]
fn second_handle_skips_redundant_recow() {
    let s = AgfsSession::new().expect("session setup");

    // Handle A writes, checkpoint, Handle A re-COWs
    fs::write(s.mnt_path("hello.txt"), "v1\n").expect("write v1");
    s.cli(&["checkpoint", "s1"]).expect("checkpoint");
    fs::write(s.mnt_path("hello.txt"), "v2\n").expect("write v2 (re-COW)");

    let status_before = s.cli(&["status"]).expect("status after v2");

    // Handle B opens and writes — should NOT create another inode
    // because dirent.checkpoint_gen already matches sbi->checkpoint_gen.
    // O_TRUNC on an already-staged file truncates the inode in-place.
    fs::write(s.mnt_path("hello.txt"), "v3\n").expect("write v3");

    // Content should be correct
    let content = fs::read_to_string(s.mnt_path("hello.txt")).expect("read");
    assert_eq!(content, "v3\n");

    // Status should still show the same number of staged changes
    let status_after = s.cli(&["status"]).expect("status after v3");
    assert_eq!(
        status_before, status_after,
        "v3 should not create additional staged changes (no redundant re-COW)"
    );
}

/// Multiple checkpoints with writes interleaved: each checkpoint preserves
/// the correct inode state.
#[test]
fn multiple_checkpoints_interleaved_writes() {
    let s = AgfsSession::new().expect("session setup");

    // v1 → checkpoint s1 → v2 → checkpoint s2 → v3
    fs::write(s.mnt_path("hello.txt"), "v1\n").expect("write v1");
    s.cli(&["checkpoint", "s1"]).expect("checkpoint s1");

    fs::write(s.mnt_path("hello.txt"), "v2\n").expect("write v2");
    s.cli(&["checkpoint", "s2"]).expect("checkpoint s2");

    fs::write(s.mnt_path("hello.txt"), "v3\n").expect("write v3");

    // Current is v3
    let content = fs::read_to_string(s.mnt_path("hello.txt")).expect("read");
    assert_eq!(content, "v3\n");

    // status --at s1: hello.txt modified (inode has v1)
    let at_s1 = s.cli(&["status", "--at", "s1"]).expect("status --at s1");
    assert!(
        at_s1.contains("hello.txt"),
        "s1 should have hello.txt: {at_s1}"
    );
    assert!(
        at_s1.contains("1 staged change"),
        "s1 should be 1 change: {at_s1}"
    );

    // status --at s2: hello.txt modified (inode has v2)
    let at_s2 = s.cli(&["status", "--at", "s2"]).expect("status --at s2");
    assert!(
        at_s2.contains("hello.txt"),
        "s2 should have hello.txt: {at_s2}"
    );
    assert!(
        at_s2.contains("1 staged change"),
        "s2 should be 1 change: {at_s2}"
    );

    // Each checkpoint state is independently verifiable via CLI — no need to count inodes.
    // The log should list both checkpoints.
    let log = s.cli(&["log"]).expect("log");
    assert!(log.contains("s1"), "log should list s1: {log}");
    assert!(log.contains("s2"), "log should list s2: {log}");
}

/// Open a file with O_APPEND (not O_TRUNC) after a checkpoint: the
/// append should trigger re-COW, preserving the pre-checkpoint content.
#[test]
fn append_after_checkpoint_triggers_recow() {
    let s = AgfsSession::new().expect("session setup");

    fs::write(s.mnt_path("hello.txt"), "line1\n").expect("write");
    s.cli(&["checkpoint", "s1"]).expect("checkpoint");

    // Append (O_APPEND, not O_TRUNC) — should re-COW before appending
    use std::io::Write;
    let mut f = fs::OpenOptions::new()
        .append(true)
        .open(s.mnt_path("hello.txt"))
        .expect("open for append");
    f.write_all(b"line2\n").expect("append");
    drop(f);

    // Current content has both lines
    let content = fs::read_to_string(s.mnt_path("hello.txt")).expect("read");
    assert_eq!(content, "line1\nline2\n");

    // The pre-checkpoint state should be preserved (re-COW triggered by append)
    let at_s1 = s.cli(&["status", "--at", "s1"]).expect("status --at s1");
    assert!(
        at_s1.contains("hello.txt"),
        "checkpoint s1 should have hello.txt preserved: {at_s1}"
    );
    assert!(
        at_s1.contains("1 staged change"),
        "checkpoint s1 should have 1 change: {at_s1}"
    );
}

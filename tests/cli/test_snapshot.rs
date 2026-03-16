use crate::helpers::AgfsSession;
use std::fs;

/// Creating a snapshot is visible via `agfs log`.
#[test]
fn snapshot_visible_in_log() {
    let s = AgfsSession::new().expect("session setup");

    fs::write(s.mnt_path("hello.txt"), "modified\n").expect("write");
    s.cli(&["snapshot", "build"]).expect("snapshot");

    let log = s.cli(&["log"]).expect("log");
    assert!(
        log.contains("build"),
        "agfs log should list the 'build' snapshot: {log}"
    );
}

/// Snapshot list shows created snapshots.
#[test]
fn snapshot_list() {
    let s = AgfsSession::new().expect("session setup");

    fs::write(s.mnt_path("hello.txt"), "v1\n").expect("write");
    s.cli(&["snapshot", "first"]).expect("snapshot 1");

    fs::write(s.mnt_path("hello.txt"), "v2\n").expect("write");
    s.cli(&["snapshot", "second"]).expect("snapshot 2");

    let output = s.cli(&["log"]).expect("snapshot list");
    assert!(output.contains("first"), "should list first: {output}");
    assert!(output.contains("second"), "should list second: {output}");
}

/// Status --at shows state at a snapshot, not the current state.
#[test]
fn status_at_snapshot() {
    let s = AgfsSession::new().expect("session setup");

    // Modify hello.txt, snapshot, then create another file
    fs::write(s.mnt_path("hello.txt"), "modified\n").expect("write");
    s.cli(&["snapshot", "checkpoint"]).expect("snapshot");

    // Create a new file after the snapshot
    fs::write(s.mnt_path("new_after_snap.txt"), "new content\n").expect("write new");

    // Status --at checkpoint should not show new_after_snap.txt
    let at_output = s
        .cli(&["status", "--at", "checkpoint"])
        .expect("status --at");
    assert!(
        at_output.contains("hello.txt"),
        "should show hello.txt: {at_output}"
    );
    assert!(
        !at_output.contains("new_after_snap.txt"),
        "should NOT show new_after_snap.txt: {at_output}"
    );

    // Regular status should show both
    let full_output = s.cli(&["status"]).expect("status");
    assert!(
        full_output.contains("hello.txt"),
        "full should show hello.txt: {full_output}"
    );
    assert!(
        full_output.contains("new_after_snap.txt"),
        "full should show new_after_snap.txt: {full_output}"
    );
}

/// Re-COW: writing after a snapshot preserves the old inode.
#[test]
fn recow_preserves_snapshot_inode() {
    let s = AgfsSession::new().expect("session setup");

    // Write v1
    fs::write(s.mnt_path("hello.txt"), "version1\n").expect("write v1");
    s.cli(&["snapshot", "v1"]).expect("snapshot v1");

    // Write v2 (should trigger re-COW)
    fs::write(s.mnt_path("hello.txt"), "version2\n").expect("write v2");

    // Current content should be v2
    let current = fs::read_to_string(s.mnt_path("hello.txt")).expect("read current");
    assert_eq!(current, "version2\n");

    // Re-COW: status --at v1 should show the snapshot state, proving inode preserved
    let at_v1 = s.cli(&["status", "--at", "v1"]).expect("status --at v1");
    assert!(
        at_v1.contains("hello.txt"),
        "should show hello.txt at v1: {at_v1}"
    );
    assert!(
        at_v1.contains("1 staged change"),
        "v1 snapshot should have exactly 1 change: {at_v1}"
    );
}

/// Commit --at only commits changes up to the snapshot.
#[test]
fn commit_at_snapshot() {
    let s = AgfsSession::new().expect("session setup");

    // Write a file, snapshot, write another
    fs::write(s.mnt_path("hello.txt"), "committed version\n").expect("write hello");
    s.cli(&["snapshot", "checkpoint"]).expect("snapshot");

    // Create a new file after snapshot
    fs::write(s.mnt_path("new_after.txt"), "staged only\n").expect("write new");

    // Commit only up to the snapshot
    s.cli(&["commit", "--at", "checkpoint"])
        .expect("commit --at");

    // hello.txt should be committed to base
    let base_content = fs::read_to_string(s.base_path("hello.txt")).expect("read base");
    assert_eq!(base_content, "committed version\n");

    // new_after.txt should still be staged (not in base from a fresh write)
    // Status should show remaining post-snapshot changes
    let remaining_status = s.cli(&["status"]).expect("status after partial commit");
    assert!(
        remaining_status.contains("new_after.txt") || remaining_status.contains("staged"),
        "should have remaining changes after partial commit: {remaining_status}"
    );
}

/// Snapshot with no name uses a timestamp.
#[test]
fn snapshot_default_name() {
    let s = AgfsSession::new().expect("session setup");
    fs::write(s.mnt_path("hello.txt"), "modified\n").expect("write");

    s.cli(&["snapshot"]).expect("snapshot with no name");

    let output = s.cli(&["log"]).expect("list");
    assert!(
        output.contains("snap-"),
        "default name should start with 'snap-': {output}"
    );
}

/// Two handles writing the same file: second handle sees the first handle's writes.
#[test]
fn two_handles_same_file_no_snapshot() {
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

    // Only one staged inode needed (no snapshot → no re-COW)
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

/// After a snapshot, a second handle's write triggers re-COW,
/// preserving the snapshot inode while both handles produce correct reads.
#[test]
fn two_handles_recow_after_snapshot() {
    let s = AgfsSession::new().expect("session setup");

    // Handle A writes v1
    fs::write(s.mnt_path("hello.txt"), "v1\n").expect("write v1");

    // Snapshot preserves v1 state
    s.cli(&["snapshot", "s1"]).expect("snapshot");

    // Handle B writes v2 (triggers re-COW: v1 inode preserved, new inode for v2)
    fs::write(s.mnt_path("hello.txt"), "v2\n").expect("write v2");

    // Current content is v2
    let content = fs::read_to_string(s.mnt_path("hello.txt")).expect("read");
    assert_eq!(content, "v2\n");

    // status --at s1 should show the v1 snapshot state (proves re-COW preserved it)
    let at_s1 = s.cli(&["status", "--at", "s1"]).expect("status --at s1");
    assert!(
        at_s1.contains("hello.txt"),
        "snapshot state should have hello.txt: {at_s1}"
    );
    assert!(
        at_s1.contains("1 staged change"),
        "snapshot s1 should have exactly 1 change: {at_s1}"
    );

    // Base still original
    let base = fs::read_to_string(s.base_path("hello.txt")).expect("read base");
    assert_eq!(base, "base content\n");
}

/// Second handle opening after another handle already re-COW'd should NOT
/// trigger a redundant re-COW (per-inode cow_snapshot_gen optimization).
#[test]
fn second_handle_skips_redundant_recow() {
    let s = AgfsSession::new().expect("session setup");

    // Handle A writes, snapshot, Handle A re-COWs
    fs::write(s.mnt_path("hello.txt"), "v1\n").expect("write v1");
    s.cli(&["snapshot", "s1"]).expect("snapshot");
    fs::write(s.mnt_path("hello.txt"), "v2\n").expect("write v2 (re-COW)");

    let status_before = s.cli(&["status"]).expect("status after v2");

    // Handle B opens and writes — should NOT create another inode
    // because override.snapshot_gen already matches sbi->snapshot_gen.
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

/// Multiple snapshots with writes interleaved: each snapshot preserves
/// the correct inode state.
#[test]
fn multiple_snapshots_interleaved_writes() {
    let s = AgfsSession::new().expect("session setup");

    // v1 → snapshot s1 → v2 → snapshot s2 → v3
    fs::write(s.mnt_path("hello.txt"), "v1\n").expect("write v1");
    s.cli(&["snapshot", "s1"]).expect("snapshot s1");

    fs::write(s.mnt_path("hello.txt"), "v2\n").expect("write v2");
    s.cli(&["snapshot", "s2"]).expect("snapshot s2");

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

    // Each snapshot state is independently verifiable via CLI — no need to count inodes.
    // The log should list both snapshots.
    let log = s.cli(&["log"]).expect("log");
    assert!(log.contains("s1"), "log should list s1: {log}");
    assert!(log.contains("s2"), "log should list s2: {log}");
}

/// Open a file with O_APPEND (not O_TRUNC) after a snapshot: the
/// append should trigger re-COW, preserving the pre-snapshot content.
#[test]
fn append_after_snapshot_triggers_recow() {
    let s = AgfsSession::new().expect("session setup");

    fs::write(s.mnt_path("hello.txt"), "line1\n").expect("write");
    s.cli(&["snapshot", "s1"]).expect("snapshot");

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

    // The pre-snapshot state should be preserved (re-COW triggered by append)
    let at_s1 = s.cli(&["status", "--at", "s1"]).expect("status --at s1");
    assert!(
        at_s1.contains("hello.txt"),
        "snapshot s1 should have hello.txt preserved: {at_s1}"
    );
    assert!(
        at_s1.contains("1 staged change"),
        "snapshot s1 should have 1 change: {at_s1}"
    );
}

use crate::helpers::AgfsSession;
use std::fs;

#[test]
fn diff_empty() {
    let Some(s) = AgfsSession::new().expect("session setup") else { return };
    let output = s.cli(&["diff"]).expect("diff");
    assert!(output.contains("No changes staged"), "output: {output}");
}

#[test]
fn diff_modified_file() {
    let Some(s) = AgfsSession::new().expect("session setup") else { return };
    fs::write(s.mnt_path("hello.txt"), "new content\n").unwrap();

    let output = s.cli(&["diff"]).expect("diff");
    assert!(output.contains("hello.txt"), "output: {output}");
    assert!(output.contains("modified"), "output: {output}");
    assert!(output.contains("+new content"), "output: {output}");
}

#[test]
fn diff_new_file() {
    let Some(s) = AgfsSession::new().expect("session setup") else { return };
    fs::write(s.mnt_path("added.txt"), "brand new\n").unwrap();

    let output = s.cli(&["diff"]).expect("diff");
    assert!(output.contains("added.txt"), "output: {output}");
    assert!(output.contains("+brand new"), "output: {output}");
}

#[test]
fn diff_deleted_file() {
    let Some(s) = AgfsSession::new().expect("session setup") else { return };
    fs::remove_file(s.mnt_path("hello.txt")).unwrap();

    let output = s.cli(&["diff"]).expect("diff");
    assert!(output.contains("hello.txt"), "output: {output}");
    assert!(
        output.contains("deleted"),
        "diff should indicate deletion: {output}"
    );
}

#[test]
fn diff_renamed_file() {
    let Some(s) = AgfsSession::new().expect("session setup") else { return };
    fs::rename(s.mnt_path("hello.txt"), s.mnt_path("moved.txt")).unwrap();

    let output = s.cli(&["diff"]).expect("diff");
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
    let Some(s) = AgfsSession::new().expect("session setup") else { return };
    fs::write(s.mnt_path("hello.txt"), "changed\n").unwrap();
    fs::write(s.mnt_path("other.txt"), "also changed\n").unwrap();

    let output = s.cli(&["diff", "hello.txt"]).expect("diff hello.txt");
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
    let Some(s) = AgfsSession::new().expect("session setup") else { return };
    fs::write(s.mnt_path("hello.txt"), "changed\n").unwrap();

    let output = s.cli(&["diff", "other.txt"]).expect("diff other.txt");
    assert!(
        output.contains("No changes staged"),
        "no matching changes: {output}"
    );
}

#[test]
fn diff_single_file_with_absolute_path() {
    let Some(s) = AgfsSession::new().expect("session setup") else { return };
    fs::write(s.mnt_path("hello.txt"), "changed\n").unwrap();

    let abs = format!("{}/hello.txt", s.root.display());
    let output = s.cli(&["diff", &abs]).expect("diff absolute path");
    assert!(
        output.contains("hello.txt"),
        "should find with absolute path: {output}"
    );
}

/// After restore, diff should only show checkpoint-state changes,
/// not post-checkpoint mutations (which are in the dead zone).
#[test]
fn diff_after_restore_excludes_dead_zone() {
    let Some(s) = AgfsSession::new().expect("session setup") else { return };
    fs::write(s.mnt_path("hello.txt"), "wanted\n").unwrap();
    s.cli(&["checkpoint", "chk1"]).expect("checkpoint");

    // Create a new file after checkpoint (becomes dead zone)
    fs::write(s.mnt_path("post_chk.txt"), "dead\n").unwrap();

    s.cli(&["restore", "chk1"]).expect("restore");

    let output = s.cli(&["diff"]).expect("diff");
    assert!(
        output.contains("hello.txt"),
        "checkpoint change should appear in diff: {output}"
    );
    assert!(
        !output.contains("post_chk.txt"),
        "dead-zone file should NOT appear in diff: {output}"
    );
}

/// Diff between two checkpoints that span a restore should still work.
#[test]
fn diff_between_checkpoints_spanning_restore() {
    let Some(s) = AgfsSession::new().expect("session setup") else { return };
    fs::write(s.mnt_path("hello.txt"), "v1\n").unwrap();
    s.cli(&["checkpoint", "chk1"]).expect("checkpoint 1");

    fs::write(s.mnt_path("hello.txt"), "v2\n").unwrap();
    fs::write(s.mnt_path("extra.txt"), "extra\n").unwrap();
    s.cli(&["checkpoint", "chk2"]).expect("checkpoint 2");

    // Restore to chk1, then work and checkpoint again
    s.cli(&["restore", "chk1"]).expect("restore");
    s.cli(&["checkpoint", "post-restore"])
        .expect("checkpoint post-restore");
    fs::write(s.mnt_path("hello.txt"), "v3\n").unwrap();
    s.cli(&["checkpoint", "chk3"]).expect("checkpoint 3");

    // Diff from chk1 to chk3 should NOT include chk2's dead-zone changes
    let output = s
        .cli(&["diff", "--from", "chk1", "--to", "chk3"])
        .expect("diff --from --to");
    assert!(
        !output.contains("extra.txt"),
        "dead-zone file should NOT appear: {output}"
    );
}

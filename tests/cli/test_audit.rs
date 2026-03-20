use crate::helpers::AgfsSession;
use std::fs;

/// `agfs audit` shows file operations.
#[test]
fn audit_shows_added_file() {
    let s = AgfsSession::new().expect("session setup");

    fs::write(s.mnt_path("hello.txt"), "content\n").expect("write");

    let output = s.cli(&["audit"]).expect("audit");
    assert!(
        output.contains("hello.txt"),
        "should show record for hello.txt: {output}"
    );
}

/// `agfs audit` shows checkpoint and restore records.
#[test]
fn audit_shows_checkpoints_and_restores() {
    let s = AgfsSession::new().expect("session setup");

    fs::write(s.mnt_path("a.txt"), "v1\n").expect("write");
    s.cli(&["checkpoint", "chk1"]).expect("checkpoint");
    fs::write(s.mnt_path("b.txt"), "v2\n").expect("write");
    s.cli(&["checkpoint", "chk2"]).expect("checkpoint");
    s.cli(&["restore", "chk1"]).expect("restore");

    let output = s.cli(&["audit"]).expect("audit");
    assert!(output.contains("chk1"), "should show chk1: {output}");
    assert!(output.contains("chk2"), "should show chk2: {output}");
    assert!(
        output.contains("restored to"),
        "should show restore record: {output}"
    );
}

/// `agfs audit` dims unreachable records with ~ prefix after restore.
#[test]
fn audit_dims_unreachable_after_restore() {
    let s = AgfsSession::new().expect("session setup");

    fs::write(s.mnt_path("a.txt"), "v1\n").expect("write");
    s.cli(&["checkpoint", "chk1"]).expect("checkpoint");
    fs::write(s.mnt_path("b.txt"), "v2\n").expect("write");
    s.cli(&["checkpoint", "chk2"]).expect("checkpoint");
    s.cli(&["restore", "chk1"]).expect("restore");

    let output = s.cli(&["audit"]).expect("audit");
    // Unreachable records (between chk1 and restore) are prefixed with ~
    let tilde_lines: Vec<&str> = output.lines().filter(|l| l.starts_with("~")).collect();
    assert!(
        !tilde_lines.is_empty(),
        "should have ~ prefixed (unreachable) lines after restore: {output}"
    );
    // The added b.txt record should be unreachable
    let has_unreachable_b = tilde_lines.iter().any(|l| l.contains("/b.txt"));
    assert!(
        has_unreachable_b,
        "b.txt should be in unreachable zone: {output}"
    );
}

/// `agfs audit --path` filters to a specific file.
#[test]
fn audit_path_filter() {
    let s = AgfsSession::new().expect("session setup");

    fs::write(s.mnt_path("a.txt"), "aaa\n").expect("write a");
    fs::write(s.mnt_path("b.txt"), "bbb\n").expect("write b");

    // CLI runs from session root; normalize_path resolves relative to cwd
    let output = s
        .cli(&["audit", "--path", "a.txt"])
        .expect("audit --path");
    assert!(output.contains("a.txt"), "should include a.txt: {output}");
    // b.txt data records should be filtered out (only structural records pass through)
    let data_lines: Vec<&str> = output.lines().filter(|l| l.contains("b.txt")).collect();
    assert!(
        data_lines.is_empty(),
        "should not show b.txt data records: {output}"
    );
}

/// `agfs audit` on a fresh session shows the initial checkpoint.
#[test]
fn audit_fresh_session() {
    let s = AgfsSession::new().expect("session setup");

    let output = s.cli(&["audit"]).expect("audit");
    // A fresh session has an initial checkpoint but no data records
    assert!(
        output.contains("initial"),
        "fresh session should show initial checkpoint: {output}"
    );
    assert!(
        !output.contains("added") && !output.contains("modified") && !output.contains("deleted"),
        "fresh session should have no data records: {output}"
    );
}

use crate::helpers::YoloSession;
use std::collections::BTreeMap;
use std::fs;
use yolofs::config::Config;
use yolofs::perm::Perm;

/// `yolo audit` shows file operations.
#[test]
fn audit_shows_added_file() {
    let s = YoloSession::new().expect("session setup");

    fs::write(s.mnt_path("hello.txt"), "content\n").expect("write");

    let output = s.cli(&["audit"]).expect("audit");
    assert!(
        output.contains("hello.txt"),
        "should show record for hello.txt: {output}"
    );
}

/// `yolo audit` shows snapshot and travel records.
#[test]
fn audit_shows_snapshots_and_travels() {
    let s = YoloSession::new().expect("session setup");

    fs::write(s.mnt_path("a.txt"), "v1\n").expect("write");
    s.cli(&["snapshot", "chk1"]).expect("snapshot");
    fs::write(s.mnt_path("b.txt"), "v2\n").expect("write");
    s.cli(&["snapshot", "chk2"]).expect("snapshot");
    s.cli(&["travel", "1"]).expect("travel");

    let output = s.cli(&["audit", "all"]).expect("audit");
    assert!(output.contains("chk1"), "should show chk1: {output}");
    assert!(output.contains("chk2"), "should show chk2: {output}");
    assert!(
        output.contains("travel"),
        "should show travel record: {output}"
    );
}

/// `yolo audit` snapshots unreachable records with (unreachable) suffix after travel.
#[test]
fn audit_dims_unreachable_after_travel() {
    let s = YoloSession::new().expect("session setup");

    fs::write(s.mnt_path("a.txt"), "v1\n").expect("write");
    s.cli(&["snapshot", "chk1"]).expect("snapshot");
    fs::write(s.mnt_path("b.txt"), "v2\n").expect("write");
    s.cli(&["snapshot", "chk2"]).expect("snapshot");
    s.cli(&["travel", "1"]).expect("travel");

    let output = s.cli(&["audit"]).expect("audit");
    let unreachable: Vec<&str> = output
        .lines()
        .filter(|l| l.contains("(unreachable)"))
        .collect();
    assert!(
        !unreachable.is_empty(),
        "should have (unreachable) lines after travel: {output}"
    );
    // The added b.txt record should be unreachable
    let has_unreachable_b = unreachable.iter().any(|l| l.contains("/b.txt"));
    assert!(
        has_unreachable_b,
        "b.txt should be in unreachable zone: {output}"
    );
}

/// `yolo audit --path` filters to a specific file.
#[test]
fn audit_path_filter() {
    let s = YoloSession::new().expect("session setup");

    fs::write(s.mnt_path("a.txt"), "aaa\n").expect("write a");
    fs::write(s.mnt_path("b.txt"), "bbb\n").expect("write b");

    // CLI runs from session root; normalize_path resolves relative to cwd
    let output = s.cli(&["audit", "--", "a.txt"]).expect("audit --path");
    assert!(output.contains("a.txt"), "should include a.txt: {output}");
    // b.txt data records should be filtered out (only structural records pass through)
    let data_lines: Vec<&str> = output.lines().filter(|l| l.contains("b.txt")).collect();
    assert!(
        data_lines.is_empty(),
        "should not show b.txt data records: {output}"
    );
}

/// `yolo audit` surfaces observational notes: a direct denial appears with
/// the target path.
#[test]
fn audit_shows_denied_note() {
    let s = YoloSession::new_with_config(Config {
        rules: BTreeMap::from([("/".into(), Perm::Deny)]),
        ..Default::default()
    })
    .expect("session setup");

    // Denied read emits G(..., d) for hello.txt (no S/D/R action).
    let _ = fs::read_to_string(s.mnt_path("hello.txt"));

    let output = s.cli(&["audit"]).expect("audit");
    assert!(
        output.contains("denied"),
        "audit should show the denied note: {output}"
    );
    assert!(
        output.contains("hello.txt"),
        "audit should name the blocked path: {output}"
    );
}

/// `yolo audit` on a fresh session shows no records.
#[test]
fn audit_fresh_session() {
    let s = YoloSession::new().expect("session setup");

    let output = s.cli(&["audit"]).expect("audit");
    // A fresh session has no records at all — the empty data answer (stdout).
    assert!(
        output.contains("(no journal records)"),
        "fresh session should report the empty journal: {output}"
    );
}

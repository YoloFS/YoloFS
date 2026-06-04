use crate::helpers::YoloSession;
use std::collections::BTreeMap;
use std::fs;
use yolofs::config::Config;
use yolofs::perm::Perm;

/// `yolofs journal` shows file operations.
#[test]
fn journal_shows_added_file() {
    let s = YoloSession::new().expect("session setup");

    fs::write(s.mnt_path("hello.txt"), "content\n").expect("write");

    let output = s.cli(&["journal"]).expect("journal");
    assert!(
        output.contains("hello.txt"),
        "should show record for hello.txt: {output}"
    );
}

/// `yolofs journal` shows snapshot and travel records.
#[test]
fn journal_shows_snapshots_and_travels() {
    let s = YoloSession::new().expect("session setup");

    fs::write(s.mnt_path("a.txt"), "v1\n").expect("write");
    s.cli(&["snapshot", "chk1"]).expect("snapshot");
    fs::write(s.mnt_path("b.txt"), "v2\n").expect("write");
    s.cli(&["snapshot", "chk2"]).expect("snapshot");
    s.cli(&["travel", "chk1"]).expect("travel");

    let output = s.cli(&["journal", "all"]).expect("journal");
    assert!(output.contains("chk1"), "should show chk1: {output}");
    assert!(output.contains("chk2"), "should show chk2: {output}");
    assert!(
        output.contains("traveled to"),
        "should show travel record: {output}"
    );
}

/// `yolofs journal` snapshots unreachable records with (unreachable) suffix after travel.
#[test]
fn journal_dims_unreachable_after_travel() {
    let s = YoloSession::new().expect("session setup");

    fs::write(s.mnt_path("a.txt"), "v1\n").expect("write");
    s.cli(&["snapshot", "chk1"]).expect("snapshot");
    fs::write(s.mnt_path("b.txt"), "v2\n").expect("write");
    s.cli(&["snapshot", "chk2"]).expect("snapshot");
    s.cli(&["travel", "chk1"]).expect("travel");

    let output = s.cli(&["journal"]).expect("journal");
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

/// `yolofs journal --path` filters to a specific file.
#[test]
fn journal_path_filter() {
    let s = YoloSession::new().expect("session setup");

    fs::write(s.mnt_path("a.txt"), "aaa\n").expect("write a");
    fs::write(s.mnt_path("b.txt"), "bbb\n").expect("write b");

    // CLI runs from session root; normalize_path resolves relative to cwd
    let output = s.cli(&["journal", "--", "a.txt"]).expect("journal --path");
    assert!(output.contains("a.txt"), "should include a.txt: {output}");
    // b.txt data records should be filtered out (only structural records pass through)
    let data_lines: Vec<&str> = output.lines().filter(|l| l.contains("b.txt")).collect();
    assert!(
        data_lines.is_empty(),
        "should not show b.txt data records: {output}"
    );
}

/// `yolofs journal` surfaces observational notes — a denied access (B note)
/// must appear as "blocked <path>". This is the complement of
/// `block_records_invisible_in_status_and_diff`: notes are hidden from
/// status/diff but visible in journal.
#[test]
fn journal_shows_blocked_note() {
    let s = YoloSession::new_with_config(Config {
        rules: BTreeMap::from([("/".into(), Perm::Deny)]),
        ..Default::default()
    })
    .expect("session setup");

    // Denied read emits a B note for hello.txt (no S/D/R action).
    let _ = fs::read_to_string(s.mnt_path("hello.txt"));

    let output = s.cli(&["journal"]).expect("journal");
    assert!(
        output.contains("blocked"),
        "journal should show the blocked note: {output}"
    );
    assert!(
        output.contains("hello.txt"),
        "journal should name the blocked path: {output}"
    );
}

/// `yolofs journal` on a fresh session shows no records.
#[test]
fn journal_fresh_session() {
    let s = YoloSession::new().expect("session setup");

    let output = s.cli(&["journal"]).expect("journal");
    // A fresh session has no records at all
    assert!(
        !output.contains("added") && !output.contains("modified") && !output.contains("deleted"),
        "fresh session should have no data records: {output}"
    );
}

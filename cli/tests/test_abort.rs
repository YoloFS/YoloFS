use crate::helpers::AgfsSession;
use crate::skip_if_not_root;
use std::fs;

#[test]
fn abort_discards_changes() {
    skip_if_not_root!();
    let s = AgfsSession::new().expect("session setup");

    fs::write(s.mnt_path("hello.txt"), "will be aborted\n").unwrap();

    // Verify staged
    let status = s.cli(&["status"]).expect("status");
    assert!(status.contains("1 staged change"), "status: {status}");

    // Abort
    let output = s.cli(&["abort"]).expect("abort");
    assert!(output.contains("staging discarded"), "output: {output}");

    // Status is clean
    let status = s.cli(&["status"]).expect("status");
    assert!(status.contains("nothing staged"), "status: {status}");

    // Base unchanged
    assert_eq!(
        fs::read_to_string(s.base_path("hello.txt")).unwrap(),
        "base content\n"
    );

    // Staging directory is empty
    let entries: Vec<_> = fs::read_dir(&s.staging)
        .expect("read staging")
        .filter_map(|e| e.ok())
        .collect();
    assert!(entries.is_empty(), "staging should be empty after abort");
}

#[test]
fn abort_when_nothing_staged() {
    skip_if_not_root!();
    let s = AgfsSession::new().expect("session setup");

    let output = s.cli(&["abort"]).expect("abort");
    assert!(output.contains("staging discarded"), "output: {output}");
}

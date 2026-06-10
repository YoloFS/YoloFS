use crate::helpers::YoloSession;
use std::fs;

#[test]
fn abort_discards_changes() {
    let s = YoloSession::new().expect("session setup");

    fs::write(s.mnt_path("hello.txt"), "will be aborted\n").unwrap();

    // Verify staged
    let status = s.cli(&["review"]).expect("status");
    assert!(status.contains("1 staged change"), "status: {status}");

    // Abort
    let output = s.cli_stderr(&["abort", "--force"]).expect("abort");
    assert!(output.contains("staging discarded"), "output: {output}");

    // Base unchanged
    assert_eq!(
        fs::read_to_string(s.base_path("hello.txt")).unwrap(),
        "base content\n"
    );
}

#[test]
fn abort_when_nothing_staged() {
    let s = YoloSession::new().expect("session setup");

    let output = s.cli_stderr(&["abort", "--force"]).expect("abort");
    assert!(output.contains("nothing to discard"), "output: {output}");
}

/// Abort after travel discards all staging (including traveled state).
#[test]
fn abort_after_travel_discards_all() {
    let s = YoloSession::new().expect("session setup");

    fs::write(s.mnt_path("hello.txt"), "v1\n").unwrap();
    s.cli(&["snapshot", "chk1"]).expect("snapshot");

    fs::write(s.mnt_path("hello.txt"), "v2\n").unwrap();

    s.cli(&["travel", "chk1"]).expect("travel");

    // Verify we're at chk1 state
    assert_eq!(fs::read_to_string(s.mnt_path("hello.txt")).unwrap(), "v1\n");

    // Abort should discard everything
    let output = s.cli_stderr(&["abort", "--force"]).expect("abort");
    assert!(output.contains("staging discarded"), "output: {output}");

    // Base should be unchanged
    assert_eq!(
        fs::read_to_string(s.base_path("hello.txt")).unwrap(),
        "base content\n"
    );

    // Status should show nothing staged
    let status = s.cli(&["review"]).expect("status");
    assert!(status.contains("no changes staged"), "status: {status}");
}

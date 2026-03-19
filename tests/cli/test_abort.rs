use crate::helpers::AgfsSession;
use std::fs;

#[test]
fn abort_discards_changes() {
    let s = AgfsSession::new().expect("session setup");

    fs::write(s.mnt_path("hello.txt"), "will be aborted\n").unwrap();

    // Verify staged
    let status = s.cli(&["status"]).expect("status");
    assert!(status.contains("1 staged change"), "status: {status}");

    // Abort
    let output = s.cli(&["abort", "--force"]).expect("abort");
    assert!(output.contains("Staging discarded"), "output: {output}");

    // Base unchanged
    assert_eq!(
        fs::read_to_string(s.base_path("hello.txt")).unwrap(),
        "base content\n"
    );
}

#[test]
fn abort_when_nothing_staged() {
    let s = AgfsSession::new().expect("session setup");

    let output = s.cli(&["abort", "--force"]).expect("abort");
    assert!(output.contains("Nothing to discard"), "output: {output}");
}

/// Abort after restore discards all staging (including restored state).
#[test]
fn abort_after_restore_discards_all() {
    let s = AgfsSession::new().expect("session setup");

    fs::write(s.mnt_path("hello.txt"), "v1\n").unwrap();
    s.cli(&["checkpoint", "chk1"]).expect("checkpoint");

    fs::write(s.mnt_path("hello.txt"), "v2\n").unwrap();

    s.cli(&["restore", "chk1"]).expect("restore");

    // Verify we're at chk1 state
    assert_eq!(
        fs::read_to_string(s.mnt_path("hello.txt")).unwrap(),
        "v1\n"
    );

    // Abort should discard everything
    let output = s.cli(&["abort", "--force"]).expect("abort");
    assert!(output.contains("Staging discarded"), "output: {output}");

    // Base should be unchanged
    assert_eq!(
        fs::read_to_string(s.base_path("hello.txt")).unwrap(),
        "base content\n"
    );

    // Status should show nothing staged
    let status = s.cli(&["status"]).expect("status");
    assert!(
        status.contains("No changes staged"),
        "status: {status}"
    );
}

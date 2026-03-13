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
    let output = s.cli(&["abort"]).expect("abort");
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

    let output = s.cli(&["abort"]).expect("abort");
    assert!(output.contains("Staging discarded"), "output: {output}");
}

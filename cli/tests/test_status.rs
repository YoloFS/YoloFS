use crate::helpers::AgfsSession;
use crate::skip_if_not_root;
use std::fs;

#[test]
fn status_empty() {
    skip_if_not_root!();
    let s = AgfsSession::new().expect("session setup");

    let output = s.cli(&["status"]).expect("status");
    assert!(output.contains("No changes staged"), "output: {output}");
}

#[test]
fn status_modified() {
    skip_if_not_root!();
    let s = AgfsSession::new().expect("session setup");

    fs::write(s.mnt_path("hello.txt"), "changed\n").unwrap();

    let output = s.cli(&["status"]).expect("status");
    assert!(output.contains("modified"), "output: {output}");
    assert!(output.contains("hello.txt"), "output: {output}");
    assert!(output.contains("1 staged change"), "output: {output}");
}

#[test]
fn status_multiple_changes() {
    skip_if_not_root!();
    let s = AgfsSession::new().expect("session setup");

    fs::write(s.mnt_path("hello.txt"), "changed\n").unwrap();
    fs::write(s.mnt_path("newfile.txt"), "new\n").unwrap();

    let output = s.cli(&["status"]).expect("status");
    assert!(output.contains("hello.txt"), "output: {output}");
    assert!(output.contains("newfile.txt"), "output: {output}");
    assert!(output.contains("2 staged change"), "output: {output}");
}

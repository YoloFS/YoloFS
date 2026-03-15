use crate::helpers::AgfsSession;
use std::fs;

#[test]
fn status_empty() {
    let s = AgfsSession::new().expect("session setup");

    let output = s.cli(&["status"]).expect("status");
    assert!(output.contains("No changes staged"), "output: {output}");
}

#[test]
fn status_modified() {
    let s = AgfsSession::new().expect("session setup");

    fs::write(s.mnt_path("hello.txt"), "changed\n").unwrap();

    let output = s.cli(&["status"]).expect("status");
    assert!(output.contains("modified"), "output: {output}");
    assert!(output.contains("hello.txt"), "output: {output}");
    assert!(output.contains("1 staged change"), "output: {output}");
}

#[test]
fn status_multiple_changes() {
    let s = AgfsSession::new().expect("session setup");

    fs::write(s.mnt_path("hello.txt"), "changed\n").unwrap();
    fs::write(s.mnt_path("newfile.txt"), "new\n").unwrap();

    let output = s.cli(&["status"]).expect("status");
    assert!(output.contains("hello.txt"), "output: {output}");
    assert!(output.contains("newfile.txt"), "output: {output}");
    assert!(output.contains("2 staged change"), "output: {output}");
}

#[test]
fn status_deleted() {
    let s = AgfsSession::new().expect("session setup");

    fs::remove_file(s.mnt_path("hello.txt")).unwrap();

    let output = s.cli(&["status"]).expect("status");
    assert!(output.contains("deleted"), "status should show deleted: {output}");
    assert!(output.contains("hello.txt"), "output: {output}");
    assert!(output.contains("1 staged change"), "output: {output}");
}

#[test]
fn status_renamed() {
    let s = AgfsSession::new().expect("session setup");

    fs::rename(s.mnt_path("hello.txt"), s.mnt_path("moved.txt")).unwrap();

    let output = s.cli(&["status"]).expect("status");
    assert!(output.contains("renamed"), "status should show renamed: {output}");
    assert!(output.contains("moved.txt"), "output: {output}");
}

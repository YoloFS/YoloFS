use crate::helpers::AgfsSession;
use std::fs;

#[test]
fn diff_empty() {
    let s = AgfsSession::new().expect("session setup");

    let output = s.cli(&["diff"]).expect("diff");
    assert!(output.contains("No changes staged"), "output: {output}");
}

#[test]
fn diff_modified_file() {
    let s = AgfsSession::new().expect("session setup");

    fs::write(s.mnt_path("hello.txt"), "new content\n").unwrap();

    let output = s.cli(&["diff"]).expect("diff");
    assert!(output.contains("hello.txt"), "output: {output}");
    assert!(output.contains("modified"), "output: {output}");
    assert!(output.contains("+new content"), "output: {output}");
}

#[test]
fn diff_new_file() {
    let s = AgfsSession::new().expect("session setup");

    fs::write(s.mnt_path("added.txt"), "brand new\n").unwrap();

    let output = s.cli(&["diff"]).expect("diff");
    assert!(output.contains("added.txt"), "output: {output}");
    assert!(output.contains("+brand new"), "output: {output}");
}

#[test]
fn diff_deleted_file() {
    let s = AgfsSession::new().expect("session setup");

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
    let s = AgfsSession::new().expect("session setup");

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

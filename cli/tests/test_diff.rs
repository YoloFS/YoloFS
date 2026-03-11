use crate::helpers::AgfsSession;
use crate::skip_if_not_root;
use std::fs;

#[test]
fn diff_empty() {
    skip_if_not_root!();
    let s = AgfsSession::new().expect("session setup");

    let output = s.cli(&["diff"]).expect("diff");
    assert!(output.contains("nothing staged"), "output: {output}");
}

#[test]
fn diff_modified_file() {
    skip_if_not_root!();
    let s = AgfsSession::new().expect("session setup");

    fs::write(s.mnt_path("hello.txt"), "new content\n").unwrap();

    let output = s.cli(&["diff"]).expect("diff");
    assert!(output.contains("diff --agfs"), "output: {output}");
    assert!(output.contains("hello.txt"), "output: {output}");
    assert!(output.contains("+new content"), "output: {output}");
}

#[test]
fn diff_new_file() {
    skip_if_not_root!();
    let s = AgfsSession::new().expect("session setup");

    fs::write(s.mnt_path("added.txt"), "brand new\n").unwrap();

    let output = s.cli(&["diff"]).expect("diff");
    assert!(output.contains("added.txt"), "output: {output}");
    assert!(output.contains("+brand new"), "output: {output}");
}

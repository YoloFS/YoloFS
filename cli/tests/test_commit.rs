use crate::helpers::AgfsSession;
use crate::skip_if_not_root;
use std::fs;

#[test]
fn commit_modified_file() {
    skip_if_not_root!();
    let s = AgfsSession::new().expect("session setup");

    fs::write(s.mnt_path("hello.txt"), "committed\n").unwrap();

    let output = s.cli(&["commit"]).expect("commit");
    assert!(output.contains("committed 1 change"), "output: {output}");

    // Base file now has the committed content
    assert_eq!(
        fs::read_to_string(s.base_path("hello.txt")).unwrap(),
        "committed\n"
    );

    // Staging is clean
    let status = s.cli(&["status"]).expect("status");
    assert!(status.contains("nothing staged"), "status: {status}");
}

#[test]
fn commit_new_file() {
    skip_if_not_root!();
    let s = AgfsSession::new().expect("session setup");

    fs::write(s.mnt_path("brandnew.txt"), "new\n").unwrap();

    let output = s.cli(&["commit"]).expect("commit");
    assert!(output.contains("committed 1 change"), "output: {output}");

    // New file now in base
    assert_eq!(
        fs::read_to_string(s.base_path("brandnew.txt")).unwrap(),
        "new\n"
    );
}

#[test]
fn commit_multiple_changes() {
    skip_if_not_root!();
    let s = AgfsSession::new().expect("session setup");

    fs::write(s.mnt_path("hello.txt"), "changed\n").unwrap();
    fs::write(s.mnt_path("newfile.txt"), "new\n").unwrap();

    let output = s.cli(&["commit"]).expect("commit");
    assert!(output.contains("committed 2 change"), "output: {output}");

    assert_eq!(
        fs::read_to_string(s.base_path("hello.txt")).unwrap(),
        "changed\n"
    );
    assert_eq!(
        fs::read_to_string(s.base_path("newfile.txt")).unwrap(),
        "new\n"
    );
}

#[test]
fn commit_nothing() {
    skip_if_not_root!();
    let s = AgfsSession::new().expect("session setup");

    let output = s.cli(&["commit"]).expect("commit");
    assert!(output.contains("nothing to commit"), "output: {output}");
}

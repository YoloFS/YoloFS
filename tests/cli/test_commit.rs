use crate::helpers::AgfsSession;
use std::fs;

#[test]
fn commit_modified_file() {
    let s = AgfsSession::new().expect("session setup");

    fs::write(s.mnt_path("hello.txt"), "committed\n").unwrap();

    let output = s.cli(&["commit"]).expect("commit");
    assert!(output.contains("Committed 1 change"), "output: {output}");

    // Base file now has the committed content
    assert_eq!(
        fs::read_to_string(s.base_path("hello.txt")).unwrap(),
        "committed\n"
    );
}

#[test]
fn commit_new_file() {
    let s = AgfsSession::new().expect("session setup");

    fs::write(s.mnt_path("brandnew.txt"), "new\n").unwrap();

    let output = s.cli(&["commit"]).expect("commit");
    assert!(output.contains("Committed 1 change"), "output: {output}");

    // New file now in base
    assert_eq!(
        fs::read_to_string(s.base_path("brandnew.txt")).unwrap(),
        "new\n"
    );
}

#[test]
fn commit_multiple_changes() {
    let s = AgfsSession::new().expect("session setup");

    fs::write(s.mnt_path("hello.txt"), "changed\n").unwrap();
    fs::write(s.mnt_path("newfile.txt"), "new\n").unwrap();

    let output = s.cli(&["commit"]).expect("commit");
    assert!(output.contains("Committed 2 change"), "output: {output}");

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
    let s = AgfsSession::new().expect("session setup");

    let output = s.cli(&["commit"]).expect("commit");
    assert!(output.contains("Nothing to commit"), "output: {output}");
}

/// Delete a directory, create a file with the same name, commit.
/// The commit should replace the directory with the file in base.
#[test]
fn commit_replace_dir_with_file() {
    let s = AgfsSession::new().expect("session setup");

    // subdir/ exists in base as a directory
    assert!(s.base_path("subdir").is_dir());

    // Remove the directory and create a file with the same name
    fs::remove_dir_all(s.mnt_path("subdir")).expect("rmdir");
    fs::write(s.mnt_path("subdir"), "now a file\n").expect("write");

    let content = fs::read_to_string(s.mnt_path("subdir")).expect("read");
    assert_eq!(content, "now a file\n");

    s.cli(&["commit"]).expect("commit");

    // Base should now have a file, not a directory
    assert!(
        !s.base_path("subdir").is_dir(),
        "base subdir should no longer be a directory after commit"
    );
    assert_eq!(
        fs::read_to_string(s.base_path("subdir")).unwrap(),
        "now a file\n",
        "base should have the file content"
    );
}

use crate::helpers::AgfsSession;
use crate::skip_if_not_root;
use std::fs;

#[test]
fn write_triggers_cow() {
    skip_if_not_root!();
    let s = AgfsSession::new().expect("session setup");

    fs::write(s.mnt_path("hello.txt"), "modified\n").expect("write through mount");

    // Read through mount sees new content
    let content = fs::read_to_string(s.mnt_path("hello.txt")).expect("read");
    assert_eq!(content, "modified\n");

    // Base file unchanged
    let base = fs::read_to_string(s.base_path("hello.txt")).expect("read base");
    assert_eq!(base, "base content\n");

    // Staging has the COW copy
    let staging = fs::read_to_string(s.staging_path("hello.txt")).expect("read staging");
    assert_eq!(staging, "modified\n");
}

#[test]
fn write_nested_file() {
    skip_if_not_root!();
    let s = AgfsSession::new().expect("session setup");

    fs::write(s.mnt_path("subdir/deep.txt"), "changed\n").expect("write nested");

    let content = fs::read_to_string(s.mnt_path("subdir/deep.txt")).unwrap();
    assert_eq!(content, "changed\n");

    // Base unchanged
    let base = fs::read_to_string(s.base_path("subdir/deep.txt")).unwrap();
    assert_eq!(base, "nested\n");
}

#[test]
fn multiple_writes_same_file() {
    skip_if_not_root!();
    let s = AgfsSession::new().expect("session setup");

    fs::write(s.mnt_path("hello.txt"), "first\n").unwrap();
    fs::write(s.mnt_path("hello.txt"), "second\n").unwrap();

    let content = fs::read_to_string(s.mnt_path("hello.txt")).unwrap();
    assert_eq!(content, "second\n");

    // Base still original
    assert_eq!(
        fs::read_to_string(s.base_path("hello.txt")).unwrap(),
        "base content\n"
    );
}

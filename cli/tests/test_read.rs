use crate::helpers::AgfsSession;
use std::fs;

#[test]
fn read_base_file() {
    let s = AgfsSession::new().expect("session setup");

    let content = fs::read_to_string(s.mnt_path("hello.txt")).expect("read hello.txt");
    assert_eq!(content, "base content\n");
}

#[test]
fn read_multiline_file() {
    let s = AgfsSession::new().expect("session setup");

    let content = fs::read_to_string(s.mnt_path("multi.txt")).expect("read multi.txt");
    assert_eq!(content, "line1\nline2\n");
}

#[test]
fn read_nested_file() {
    let s = AgfsSession::new().expect("session setup");

    let content = fs::read_to_string(s.mnt_path("subdir/deep.txt")).expect("read deep.txt");
    assert_eq!(content, "nested\n");
}

#[test]
fn read_nonexistent_fails() {
    let s = AgfsSession::new().expect("session setup");

    assert!(fs::read_to_string(s.mnt_path("nonexistent.txt")).is_err());
}

#[test]
fn stat_file() {
    let s = AgfsSession::new().expect("session setup");

    let meta = fs::metadata(s.mnt_path("hello.txt")).expect("stat hello.txt");
    assert!(meta.is_file());
    assert_eq!(meta.len(), 13); // "base content\n"
}

#[test]
fn readdir() {
    let s = AgfsSession::new().expect("session setup");

    let entries: Vec<String> = fs::read_dir(s.mnt_path(""))
        .expect("readdir")
        .filter_map(|e| e.ok().map(|e| e.file_name().to_string_lossy().to_string()))
        .collect();

    assert!(entries.contains(&"hello.txt".to_string()));
    assert!(entries.contains(&"multi.txt".to_string()));
    assert!(entries.contains(&"subdir".to_string()));
}

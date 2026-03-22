use crate::helpers::AgfsSession;
use std::fs;

#[test]
fn read_base_file() {
    let s = AgfsSession::new().expect("session setup");
    s.run_in_namespace(|| {
        let content = fs::read_to_string(s.mnt_path("hello.txt")).expect("read hello.txt");
        assert_eq!(content, "base content\n");
    });
}

#[test]
fn read_multiline_file() {
    let s = AgfsSession::new().expect("session setup");
    s.run_in_namespace(|| {
        let content = fs::read_to_string(s.mnt_path("multi.txt")).expect("read multi.txt");
        assert_eq!(content, "line1\nline2\n");
    });
}

#[test]
fn read_nested_file() {
    let s = AgfsSession::new().expect("session setup");
    s.run_in_namespace(|| {
        let content = fs::read_to_string(s.mnt_path("subdir/deep.txt")).expect("read deep.txt");
        assert_eq!(content, "nested\n");
    });
}

#[test]
fn read_nonexistent_fails() {
    let s = AgfsSession::new().expect("session setup");
    s.run_in_namespace(|| {
        assert!(fs::read_to_string(s.mnt_path("nonexistent.txt")).is_err());
    });
}

#[test]
fn stat_file() {
    let s = AgfsSession::new().expect("session setup");
    s.run_in_namespace(|| {
        let meta = fs::metadata(s.mnt_path("hello.txt")).expect("stat hello.txt");
        assert!(meta.is_file());
        assert_eq!(meta.len(), 13); // "base content\n"
    });
}

#[test]
fn deeply_nested_path() {
    let s = AgfsSession::new().expect("session setup");
    s.run_in_namespace(|| {
        let mut path = String::new();
        for i in 0..10 {
            if !path.is_empty() {
                path.push('/');
            }
            path.push_str(&format!("level_{i}"));
        }
        fs::create_dir_all(s.mnt_path(&path)).expect("mkdir -p");
        let file_path = format!("{path}/deep.txt");
        fs::write(s.mnt_path(&file_path), "deep content\n").expect("write");

        let content = fs::read_to_string(s.mnt_path(&file_path)).expect("read");
        assert_eq!(content, "deep content\n");
    });
}

use crate::helpers::AgfsSession;
use std::fs::OpenOptions;
use std::io::Write;
use std::{fs, io::Read};

#[test]
fn truncating_write() {
    let s = AgfsSession::new().expect("session setup");

    // Truncating write (O_TRUNC) via create-new
    fs::write(s.mnt_path("hello.txt"), "truncated\n").unwrap();

    assert_eq!(
        fs::read_to_string(s.mnt_path("hello.txt")).unwrap(),
        "truncated\n"
    );
    // Base unchanged
    assert_eq!(
        fs::read_to_string(s.base_path("hello.txt")).unwrap(),
        "base content\n"
    );
}

#[test]
fn append_to_file() {
    let s = AgfsSession::new().expect("session setup");

    // Open for append
    let mut f = OpenOptions::new()
        .append(true)
        .open(s.mnt_path("multi.txt"))
        .expect("open for append");
    write!(f, "line3\n").expect("append");
    drop(f);

    let mut content = String::new();
    fs::File::open(s.mnt_path("multi.txt"))
        .unwrap()
        .read_to_string(&mut content)
        .unwrap();
    assert_eq!(content, "line1\nline2\nline3\n");

    // Base unchanged
    assert_eq!(
        fs::read_to_string(s.base_path("multi.txt")).unwrap(),
        "line1\nline2\n"
    );
}

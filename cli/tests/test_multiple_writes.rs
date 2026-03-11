use crate::helpers::AgfsSession;
use crate::skip_if_not_root;
use std::fs;
use std::fs::OpenOptions;
use std::io::Write;

#[test]
fn sequential_writes_to_different_files() {
    skip_if_not_root!();
    let s = AgfsSession::new().expect("session setup");

    fs::write(s.mnt_path("hello.txt"), "mod1\n").unwrap();
    fs::write(s.mnt_path("multi.txt"), "mod2\n").unwrap();
    fs::write(s.mnt_path("subdir/deep.txt"), "mod3\n").unwrap();

    assert_eq!(fs::read_to_string(s.mnt_path("hello.txt")).unwrap(), "mod1\n");
    assert_eq!(fs::read_to_string(s.mnt_path("multi.txt")).unwrap(), "mod2\n");
    assert_eq!(fs::read_to_string(s.mnt_path("subdir/deep.txt")).unwrap(), "mod3\n");

    // All bases unchanged
    assert_eq!(fs::read_to_string(s.base_path("hello.txt")).unwrap(), "base content\n");
    assert_eq!(fs::read_to_string(s.base_path("multi.txt")).unwrap(), "line1\nline2\n");
    assert_eq!(fs::read_to_string(s.base_path("subdir/deep.txt")).unwrap(), "nested\n");
}

#[test]
fn overwrite_then_read() {
    skip_if_not_root!();
    let s = AgfsSession::new().expect("session setup");

    // Write, read, overwrite, read — should always see latest
    fs::write(s.mnt_path("hello.txt"), "v1\n").unwrap();
    assert_eq!(fs::read_to_string(s.mnt_path("hello.txt")).unwrap(), "v1\n");

    fs::write(s.mnt_path("hello.txt"), "v2\n").unwrap();
    assert_eq!(fs::read_to_string(s.mnt_path("hello.txt")).unwrap(), "v2\n");

    fs::write(s.mnt_path("hello.txt"), "v3 is longer content\n").unwrap();
    assert_eq!(
        fs::read_to_string(s.mnt_path("hello.txt")).unwrap(),
        "v3 is longer content\n"
    );
}

#[test]
fn append_multiple_times() {
    skip_if_not_root!();
    let s = AgfsSession::new().expect("session setup");

    for i in 0..5 {
        let mut f = OpenOptions::new()
            .append(true)
            .open(s.mnt_path("multi.txt"))
            .unwrap();
        writeln!(f, "appended-{i}").unwrap();
    }

    let content = fs::read_to_string(s.mnt_path("multi.txt")).unwrap();
    assert!(content.starts_with("line1\nline2\n"));
    for i in 0..5 {
        assert!(content.contains(&format!("appended-{i}")), "missing appended-{i}");
    }
}

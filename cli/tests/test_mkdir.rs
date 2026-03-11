use crate::helpers::AgfsSession;
use crate::skip_if_not_root;
use std::fs;

#[test]
fn mkdir_through_mount() {
    skip_if_not_root!();
    let s = AgfsSession::new().expect("session setup");

    fs::create_dir(s.mnt_path("newdir")).expect("mkdir");
    assert!(s.mnt_path("newdir").is_dir());
}

#[test]
fn mkdir_nested() {
    skip_if_not_root!();
    let s = AgfsSession::new().expect("session setup");

    fs::create_dir_all(s.mnt_path("a/b/c")).expect("mkdir -p");
    assert!(s.mnt_path("a/b/c").is_dir());

    // Write a file in the new nested dir
    fs::write(s.mnt_path("a/b/c/file.txt"), "deep\n").unwrap();
    assert_eq!(
        fs::read_to_string(s.mnt_path("a/b/c/file.txt")).unwrap(),
        "deep\n"
    );
}

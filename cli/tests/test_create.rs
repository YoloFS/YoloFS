use crate::helpers::AgfsSession;
use crate::skip_if_not_root;
use std::fs;

#[test]
fn create_new_file() {
    skip_if_not_root!();
    let s = AgfsSession::new().expect("session setup");

    fs::write(s.mnt_path("brandnew.txt"), "new content\n").expect("create new file");

    // Readable through mount
    let content = fs::read_to_string(s.mnt_path("brandnew.txt")).unwrap();
    assert_eq!(content, "new content\n");
}

#[test]
fn create_file_in_new_subdir() {
    skip_if_not_root!();
    let s = AgfsSession::new().expect("session setup");

    // Create a nested file in a new directory through the mount
    fs::create_dir_all(s.mnt_path("newdir")).expect("mkdir");
    fs::write(s.mnt_path("newdir/file.txt"), "deep new\n").expect("write");

    let content = fs::read_to_string(s.mnt_path("newdir/file.txt")).unwrap();
    assert_eq!(content, "deep new\n");
}

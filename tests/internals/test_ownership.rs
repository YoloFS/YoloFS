use crate::helpers::AgfsSession;
use std::fs;
use std::os::unix::fs::MetadataExt;

use super::helpers::{ino_for, inode_path, tree};

/// A newly created file's inode in the store should be owned by the
/// calling user, not root.
#[test]
fn staged_file_owned_by_caller() {
    let s = AgfsSession::new().expect("session");

    s.run_in_namespace(|| {
        let uid = nix::unistd::getuid().as_raw();

        fs::write(s.mnt_path("owned.txt"), "mine").expect("create");

        let ch = tree(&s);
        let ino = ino_for(&ch, "/owned.txt");
        let meta = fs::metadata(inode_path(&s, ino)).expect("stat inode");
        assert_eq!(
            meta.uid(),
            uid,
            "staged file inode should be owned by uid {uid}, got {}",
            meta.uid()
        );
    });
}

/// A newly created directory's inode in the store should be owned by the
/// calling user, not root.
#[test]
fn staged_dir_owned_by_caller() {
    let s = AgfsSession::new().expect("session");

    s.run_in_namespace(|| {
        let uid = nix::unistd::getuid().as_raw();

        fs::create_dir(s.mnt_path("owneddir")).expect("mkdir");

        let ch = tree(&s);
        let ino = ino_for(&ch, "/owneddir");
        let meta = fs::metadata(inode_path(&s, ino)).expect("stat inode dir");
        assert_eq!(
            meta.uid(),
            uid,
            "staged dir inode should be owned by uid {uid}, got {}",
            meta.uid()
        );
    });
}

/// A COW file's inode should be owned by the user who triggered the write,
/// not root.
#[test]
fn cow_file_owned_by_caller() {
    let s = AgfsSession::new().expect("session");

    s.run_in_namespace(|| {
        let uid = nix::unistd::getuid().as_raw();

        fs::write(s.mnt_path("hello.txt"), "cow data").expect("COW write");

        let ch = tree(&s);
        let ino = ino_for(&ch, "/hello.txt");
        let meta = fs::metadata(inode_path(&s, ino)).expect("stat COW inode");
        assert_eq!(
            meta.uid(),
            uid,
            "COW inode should be owned by uid {uid}, got {}",
            meta.uid()
        );
    });
}

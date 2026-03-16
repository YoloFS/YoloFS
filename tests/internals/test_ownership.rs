use crate::helpers::AgfsSession;
use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt};

use nix::sys::wait::{WaitStatus, waitpid};
use nix::unistd::{ForkResult, Pid, Uid, fork, setuid};

use super::helpers::{changes, ino_for, inode_path};

fn assert_child_ok(child: Pid, msg: &str) {
    match waitpid(child, None).expect("waitpid") {
        WaitStatus::Exited(_, 0) => {}
        WaitStatus::Exited(_, code) => panic!("{msg}: child exited with code {code}"),
        other => panic!("{msg}: unexpected child status: {other:?}"),
    }
}

fn make_accessible(s: &AgfsSession) {
    fs::set_permissions(&s.root, fs::Permissions::from_mode(0o777)).unwrap();
}

/// A newly created file's inode in the store should be owned by the
/// creating user, not root.
#[test]
fn staged_file_owned_by_caller() {
    let s = AgfsSession::new().expect("session");
    make_accessible(&s);

    let path = s.mnt_path("owned.txt");

    match unsafe { fork() }.expect("fork") {
        ForkResult::Child => {
            setuid(Uid::from_raw(1000)).expect("setuid");
            let ok = fs::write(&path, "mine").is_ok();
            std::process::exit(if ok { 0 } else { 1 });
        }
        ForkResult::Parent { child } => {
            assert_child_ok(child, "create as uid 1000");
        }
    }

    let ch = changes(&s);
    let ino = ino_for(&ch, "/owned.txt");
    let meta = fs::metadata(inode_path(&s, ino)).expect("stat inode");
    assert_eq!(
        meta.uid(),
        1000,
        "staged file inode should be owned by uid 1000, got {}",
        meta.uid()
    );
}

/// A newly created directory's inode in the store should be owned by the
/// creating user, not root.
#[test]
fn staged_dir_owned_by_caller() {
    let s = AgfsSession::new().expect("session");
    make_accessible(&s);

    let dir = s.mnt_path("owneddir");

    match unsafe { fork() }.expect("fork") {
        ForkResult::Child => {
            setuid(Uid::from_raw(1000)).expect("setuid");
            let ok = fs::create_dir(&dir).is_ok();
            std::process::exit(if ok { 0 } else { 1 });
        }
        ForkResult::Parent { child } => {
            assert_child_ok(child, "mkdir as uid 1000");
        }
    }

    let ch = changes(&s);
    let ino = ino_for(&ch, "/owneddir");
    let meta = fs::metadata(inode_path(&s, ino)).expect("stat inode dir");
    assert_eq!(
        meta.uid(),
        1000,
        "staged dir inode should be owned by uid 1000, got {}",
        meta.uid()
    );
}

/// A COW file's inode should be owned by the user who triggered the write,
/// not root.
#[test]
fn cow_file_owned_by_caller() {
    let s = AgfsSession::new().expect("session");
    make_accessible(&s);

    let path = s.mnt_path("hello.txt");

    match unsafe { fork() }.expect("fork") {
        ForkResult::Child => {
            setuid(Uid::from_raw(1000)).expect("setuid");
            let ok = fs::write(&path, "cow by 1000").is_ok();
            std::process::exit(if ok { 0 } else { 1 });
        }
        ForkResult::Parent { child } => {
            assert_child_ok(child, "COW write as uid 1000");
        }
    }

    let ch = changes(&s);
    let ino = ino_for(&ch, "/hello.txt");
    let meta = fs::metadata(inode_path(&s, ino)).expect("stat COW inode");
    assert_eq!(
        meta.uid(),
        1000,
        "COW inode should be owned by uid 1000, got {}",
        meta.uid()
    );
}

use crate::helpers::AgfsSession;
use std::fs;
use std::os::unix::fs::PermissionsExt;

use nix::sys::wait::{WaitStatus, waitpid};
use nix::unistd::{ForkResult, Pid, Uid, fork, setuid};

// ── Helpers ──────────────────────────────────────────────────────────

/// Check that a forked child exited with code 0.
fn assert_child_ok(child: Pid, msg: &str) {
    match waitpid(child, None).expect("waitpid") {
        WaitStatus::Exited(_, 0) => {}
        WaitStatus::Exited(_, code) => panic!("{msg}: child exited with code {code}"),
        other => panic!("{msg}: unexpected child status: {other:?}"),
    }
}

/// Make the session root traversable by non-root users.
fn make_accessible(s: &AgfsSession) {
    fs::set_permissions(&s.root, fs::Permissions::from_mode(0o777)).unwrap();
}

// ── Tests ────────────────────────────────────────────────────────────

/// Non-root user can create a new file and read it back.
#[test]
fn non_root_create_and_read_file() {
    let s = AgfsSession::new().expect("session");
    make_accessible(&s);

    let path = s.mnt_path("created.txt");

    match unsafe { fork() }.expect("fork") {
        ForkResult::Child => {
            setuid(Uid::from_raw(1000)).expect("setuid");
            if fs::write(&path, "hello from 1000\n").is_err() {
                std::process::exit(1); // create failed
            }
            match fs::read_to_string(&path) {
                Ok(s) if s == "hello from 1000\n" => std::process::exit(0),
                Ok(_) => std::process::exit(3),  // wrong content
                Err(_) => std::process::exit(2), // read failed
            }
        }
        ForkResult::Parent { child } => {
            assert_child_ok(child, "non-root create + read");
        }
    }
}

/// Non-root user can modify a base file (triggers COW) and read it back.
#[test]
fn non_root_write_existing_file() {
    let s = AgfsSession::new().expect("session");
    make_accessible(&s);

    let path = s.mnt_path("hello.txt");

    match unsafe { fork() }.expect("fork") {
        ForkResult::Child => {
            setuid(Uid::from_raw(1000)).expect("setuid");
            if fs::write(&path, "modified by 1000\n").is_err() {
                std::process::exit(1); // write (COW) failed
            }
            match fs::read_to_string(&path) {
                Ok(s) if s == "modified by 1000\n" => std::process::exit(0),
                Ok(_) => std::process::exit(3),
                Err(_) => std::process::exit(2),
            }
        }
        ForkResult::Parent { child } => {
            assert_child_ok(child, "non-root COW write + read");
            // Base file should be unchanged
            assert_eq!(
                fs::read_to_string(s.base_path("hello.txt")).unwrap(),
                "base content\n"
            );
        }
    }
}

/// Non-root user can delete a file.
#[test]
fn non_root_delete_file() {
    let s = AgfsSession::new().expect("session");
    make_accessible(&s);

    let path = s.mnt_path("hello.txt");

    match unsafe { fork() }.expect("fork") {
        ForkResult::Child => {
            setuid(Uid::from_raw(1000)).expect("setuid");
            let ok = fs::remove_file(&path).is_ok();
            std::process::exit(if ok { 0 } else { 1 });
        }
        ForkResult::Parent { child } => {
            assert_child_ok(child, "non-root delete");
            assert!(!s.mnt_path("hello.txt").exists());
        }
    }
}

/// Non-root user can rename a file.
#[test]
fn non_root_rename_file() {
    let s = AgfsSession::new().expect("session");
    make_accessible(&s);

    let src = s.mnt_path("hello.txt");
    let dst = s.mnt_path("moved.txt");

    match unsafe { fork() }.expect("fork") {
        ForkResult::Child => {
            setuid(Uid::from_raw(1000)).expect("setuid");
            let ok = fs::rename(&src, &dst).is_ok();
            std::process::exit(if ok { 0 } else { 1 });
        }
        ForkResult::Parent { child } => {
            assert_child_ok(child, "non-root rename");
            assert!(!s.mnt_path("hello.txt").exists());
            assert_eq!(
                fs::read_to_string(s.mnt_path("moved.txt")).unwrap(),
                "base content\n"
            );
        }
    }
}

/// Non-root user can create a symlink and read through it.
#[test]
fn non_root_create_symlink() {
    let s = AgfsSession::new().expect("session");
    make_accessible(&s);

    let link = s.mnt_path("link.txt");

    match unsafe { fork() }.expect("fork") {
        ForkResult::Child => {
            setuid(Uid::from_raw(1000)).expect("setuid");
            if std::os::unix::fs::symlink("hello.txt", &link).is_err() {
                std::process::exit(1); // symlink failed
            }
            match fs::read_to_string(&link) {
                Ok(s) if s == "base content\n" => std::process::exit(0),
                Ok(_) => std::process::exit(3),
                Err(_) => std::process::exit(2), // read through link failed
            }
        }
        ForkResult::Parent { child } => {
            assert_child_ok(child, "non-root symlink + read");
        }
    }
}

/// Non-root user can create a directory.
#[test]
fn non_root_mkdir() {
    let s = AgfsSession::new().expect("session");
    make_accessible(&s);

    let dir = s.mnt_path("newdir");

    match unsafe { fork() }.expect("fork") {
        ForkResult::Child => {
            setuid(Uid::from_raw(1000)).expect("setuid");
            let ok = fs::create_dir(&dir).is_ok();
            std::process::exit(if ok { 0 } else { 1 });
        }
        ForkResult::Parent { child } => {
            assert_child_ok(child, "non-root mkdir");
        }
    }
}

/// Non-root user can create nested directories and write files inside.
#[test]
fn non_root_mkdir_nested_write() {
    let s = AgfsSession::new().expect("session");
    make_accessible(&s);

    let dir = s.mnt_path("a");

    match unsafe { fork() }.expect("fork") {
        ForkResult::Child => {
            setuid(Uid::from_raw(1000)).expect("setuid");
            if fs::create_dir(&dir).is_err() {
                std::process::exit(1);
            }
            let inner = dir.join("b");
            if fs::create_dir(&inner).is_err() {
                std::process::exit(2);
            }
            if fs::write(inner.join("deep.txt"), "deep data").is_err() {
                std::process::exit(3);
            }
            match fs::read_to_string(inner.join("deep.txt")) {
                Ok(s) if s == "deep data" => std::process::exit(0),
                _ => std::process::exit(4),
            }
        }
        ForkResult::Parent { child } => {
            assert_child_ok(child, "non-root nested mkdir + write");
        }
    }
}

/// Non-root user can list a directory.
#[test]
fn non_root_readdir() {
    let s = AgfsSession::new().expect("session");
    make_accessible(&s);

    let dir = s.mnt_path("");

    match unsafe { fork() }.expect("fork") {
        ForkResult::Child => {
            setuid(Uid::from_raw(1000)).expect("setuid");
            match fs::read_dir(&dir) {
                Ok(entries) => {
                    let names: Vec<_> = entries.filter_map(|e| e.ok()).collect();
                    std::process::exit(if names.is_empty() { 2 } else { 0 });
                }
                Err(_) => std::process::exit(1),
            }
        }
        ForkResult::Parent { child } => {
            assert_child_ok(child, "non-root readdir");
        }
    }
}

/// Non-root user can remove a directory.
#[test]
fn non_root_rmdir() {
    let s = AgfsSession::new().expect("session");
    make_accessible(&s);

    let dir = s.mnt_path("subdir");

    // First remove the file inside (as root, since test setup created it)
    fs::remove_file(s.mnt_path("subdir/deep.txt")).expect("remove deep.txt");

    match unsafe { fork() }.expect("fork") {
        ForkResult::Child => {
            setuid(Uid::from_raw(1000)).expect("setuid");
            let ok = fs::remove_dir(&dir).is_ok();
            std::process::exit(if ok { 0 } else { 1 });
        }
        ForkResult::Parent { child } => {
            assert_child_ok(child, "non-root rmdir");
        }
    }
}

/// Non-root user can create a file, then modify it in a second open.
#[test]
fn non_root_create_then_modify() {
    let s = AgfsSession::new().expect("session");
    make_accessible(&s);

    let path = s.mnt_path("staged.txt");

    match unsafe { fork() }.expect("fork") {
        ForkResult::Child => {
            setuid(Uid::from_raw(1000)).expect("setuid");
            if fs::write(&path, "first").is_err() {
                std::process::exit(1);
            }
            if fs::write(&path, "second").is_err() {
                std::process::exit(2);
            }
            match fs::read_to_string(&path) {
                Ok(s) if s == "second" => std::process::exit(0),
                _ => std::process::exit(3),
            }
        }
        ForkResult::Parent { child } => {
            assert_child_ok(child, "non-root create then modify");
        }
    }
}

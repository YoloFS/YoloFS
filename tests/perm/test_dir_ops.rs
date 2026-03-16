use crate::helpers::AgfsSession;
use agfs::config::{Config, Perm};
use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};

// ── Directory ops bypass agfs permission (inode.c: agfs_permission
//    delegates to lower FS for non-regular files) ──

/// mkdir should succeed even under a deny rule because directory ops
/// are checked against the lower FS, not agfs permission rules.
#[test]
fn mkdir_allowed_under_deny() {
    let s = AgfsSession::new_with_config(Config {
        ask_default: Some(Perm::Deny),
        rules: BTreeMap::from([("/".into(), Perm::Deny)]),
        ..Default::default()
    })
    .expect("session setup");

    fs::create_dir(s.mnt_path("newdir")).expect("mkdir should succeed: dir ops bypass agfs perm");
}

/// unlink should succeed under allow-ro because inode removal is a
/// directory operation and doesn't go through agfs_permission on the file.
#[test]
fn unlink_allowed_under_allow_ro() {
    let s = AgfsSession::new_with_config(Config {
        ask_default: Some(Perm::Deny),
        rules: BTreeMap::from([("/".into(), Perm::AllowRo)]),
        ..Default::default()
    })
    .expect("session setup");

    // unlink goes through agfs_unlink which adds a DELETED override
    fs::remove_file(s.mnt_path("hello.txt"))
        .expect("unlink should succeed: it is a dir op on the parent");
}

/// symlink creation should succeed under deny because symlink is a
/// directory inode operation.
#[test]
fn symlink_allowed_under_deny() {
    let s = AgfsSession::new_with_config(Config {
        ask_default: Some(Perm::Deny),
        rules: BTreeMap::from([("/".into(), Perm::Deny)]),
        ..Default::default()
    })
    .expect("session setup");

    std::os::unix::fs::symlink("hello.txt", s.mnt_path("link.txt"))
        .expect("symlink should succeed: dir ops bypass agfs perm");
}

/// rmdir should succeed even under deny because it is a directory inode op.
#[test]
fn rmdir_allowed_under_deny() {
    let s = AgfsSession::new_with_config(Config {
        ask_default: Some(Perm::Deny),
        rules: BTreeMap::from([("/".into(), Perm::Deny)]),
        ..Default::default()
    })
    .expect("session setup");

    fs::remove_dir(s.mnt_path("subdir")).expect("rmdir should succeed: dir ops bypass agfs perm");
}

/// rename should succeed under deny because it is a directory inode op.
#[test]
fn rename_allowed_under_deny() {
    let s = AgfsSession::new_with_config(Config {
        ask_default: Some(Perm::Deny),
        rules: BTreeMap::from([("/".into(), Perm::Deny)]),
        ..Default::default()
    })
    .expect("session setup");

    fs::rename(s.mnt_path("hello.txt"), s.mnt_path("renamed.txt"))
        .expect("rename should succeed: dir ops bypass agfs perm");
}

/// file creation should succeed under deny because it is a directory inode op.
#[test]
fn create_allowed_under_deny() {
    let s = AgfsSession::new_with_config(Config {
        ask_default: Some(Perm::Deny),
        rules: BTreeMap::from([("/".into(), Perm::Deny)]),
        ..Default::default()
    })
    .expect("session setup");

    // O_CREAT goes through agfs_create (dir op) then agfs_open checks perm.
    // fs::write uses O_WRONLY|O_CREAT|O_TRUNC, so the create (dir op) succeeds
    // but the open (file op) should fail under deny.
    let result = fs::write(s.mnt_path("newfile.txt"), "data");
    assert!(
        result.is_err(),
        "write to new file should fail under deny (open is gated)"
    );

    // The file was created in inode store (dir op succeeded). Verify via status.
    let status = s.cli(&["status"]).unwrap();
    assert!(
        status.contains("newfile.txt"),
        "status should show the created file: {status}"
    );
}

/// Regression: a non-root user must be able to write into a directory they
/// just created inside the sandbox.
///
/// Real-world trigger: `make install-third-party` runs autoconf, which uses
/// Perl's File::Temp to create a private temp dir (mode 0700) under /tmp.
/// The mkdir succeeds but the subsequent chdir/write into it fails with
/// EACCES.
///
/// Root cause: `agfs_permission` (inode.c:231) delegates directory permission
/// checks to the lower filesystem instead of going through agfs rules.
/// Staged inodes are always created under root credentials
/// (`override_creds(sbi->creator_cred)`), so a directory staged by a
/// non-root user ends up root-owned — and the creating user cannot write
/// into it.
#[test]
fn non_root_mkdir_then_write_inside() {
    use nix::sys::wait::{WaitStatus, waitpid};
    use nix::unistd::{ForkResult, Uid, fork, setuid};

    let s = AgfsSession::new_with_config(Config {
        permission: true,
        rules: BTreeMap::from([("/".into(), Perm::Allow)]),
        ..Default::default()
    })
    .expect("session setup");

    // Allow non-root to traverse the temp path to the mount point.
    fs::set_permissions(&s.root, fs::Permissions::from_mode(0o777)).unwrap();

    let newdir = s.mnt_path("newdir");

    match unsafe { fork() }.expect("fork") {
        ForkResult::Child => {
            setuid(Uid::from_raw(1000)).expect("setuid 1000");
            // Mode 0700: the user intends to own this dir privately.
            // This mirrors Perl's File::Temp, which is the real-world trigger.
            if std::fs::DirBuilder::new()
                .mode(0o700)
                .create(&newdir)
                .is_err()
            {
                std::process::exit(1);
            }
            // Write inside — requires write+exec on the new dir.
            // With the bug the staged inode is root-owned 0700, so
            // agfs_permission (delegating dir checks to lower FS) returns EACCES.
            let ok = std::fs::write(newdir.join("file.txt"), "data").is_ok();
            std::process::exit(if ok { 0 } else { 2 });
        }
        ForkResult::Parent { child } => match waitpid(child, None).expect("waitpid") {
            WaitStatus::Exited(_, 0) => {}
            WaitStatus::Exited(_, 2) => panic!(
                "non-root user could mkdir but not write inside it: \
                     staged inode is root-owned, agfs_permission delegates \
                     directory checks to lower FS (inode.c:231)"
            ),
            other => panic!("unexpected child status: {other:?}"),
        },
    }
}

/// Listing a directory's contents should work even under deny
/// (readdir is a directory operation, not a regular file open).
#[test]
fn readdir_allowed_under_deny() {
    let s = AgfsSession::new_with_config(Config {
        ask_default: Some(Perm::Deny),
        rules: BTreeMap::from([("/".into(), Perm::Deny)]),
        ..Default::default()
    })
    .expect("session setup");

    let entries: Vec<_> = fs::read_dir(s.mnt_path(""))
        .expect("readdir should succeed under deny")
        .collect();
    assert!(!entries.is_empty(), "directory should have entries");
}

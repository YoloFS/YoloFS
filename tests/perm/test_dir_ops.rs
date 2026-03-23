use crate::helpers::AgfsSession;
use agfs::config::{Config, Perm};
use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;

// ── Directory ops bypass agfs permission (inode.c: agfs_permission
//    delegates to lower FS for non-regular files) ──

/// mkdir should succeed even under a deny rule because directory ops
/// are checked against the lower FS, not agfs permission rules.
#[test]
fn mkdir_allowed_under_deny() {
    let Some(s) = AgfsSession::new_with_config(Config {
        ask_default: Some(Perm::Deny),
        rules: BTreeMap::from([("/".into(), Perm::Deny)]),
        ..Default::default()
    })
    .expect("session setup") else { return };
    fs::create_dir(s.mnt_path("newdir"))
        .expect("mkdir should succeed: dir ops bypass agfs perm");
}

/// unlink should succeed under allow-ro because inode removal is a
/// directory operation and doesn't go through agfs_permission on the file.
#[test]
fn unlink_allowed_under_allow_ro() {
    let Some(s) = AgfsSession::new_with_config(Config {
        ask_default: Some(Perm::Deny),
        rules: BTreeMap::from([("/".into(), Perm::AllowRo)]),
        ..Default::default()
    })
    .expect("session setup") else { return };
    // unlink goes through agfs_unlink which adds a DELETED dirent
    fs::remove_file(s.mnt_path("hello.txt"))
        .expect("unlink should succeed: it is a dir op on the parent");
}

/// symlink creation should succeed under deny because symlink is a
/// directory inode operation.
#[test]
fn symlink_allowed_under_deny() {
    let Some(s) = AgfsSession::new_with_config(Config {
        ask_default: Some(Perm::Deny),
        rules: BTreeMap::from([("/".into(), Perm::Deny)]),
        ..Default::default()
    })
    .expect("session setup") else { return };
    std::os::unix::fs::symlink("hello.txt", s.mnt_path("link.txt"))
        .expect("symlink should succeed: dir ops bypass agfs perm");
}

/// rmdir should succeed even under deny because it is a directory inode op.
#[test]
fn rmdir_allowed_under_deny() {
    let Some(s) = AgfsSession::new_with_config(Config {
        ask_default: Some(Perm::Deny),
        rules: BTreeMap::from([("/".into(), Perm::Deny)]),
        ..Default::default()
    })
    .expect("session setup") else { return };
    fs::remove_dir(s.mnt_path("subdir"))
        .expect("rmdir should succeed: dir ops bypass agfs perm");
}

/// rename should succeed under deny because it is a directory inode op.
#[test]
fn rename_allowed_under_deny() {
    let Some(s) = AgfsSession::new_with_config(Config {
        ask_default: Some(Perm::Deny),
        rules: BTreeMap::from([("/".into(), Perm::Deny)]),
        ..Default::default()
    })
    .expect("session setup") else { return };
    fs::rename(s.mnt_path("hello.txt"), s.mnt_path("renamed.txt"))
        .expect("rename should succeed: dir ops bypass agfs perm");
}

/// file creation should succeed under deny because it is a directory inode op.
#[test]
fn create_allowed_under_deny() {
    let Some(s) = AgfsSession::new_with_config(Config {
        ask_default: Some(Perm::Deny),
        rules: BTreeMap::from([("/".into(), Perm::Deny)]),
        ..Default::default()
    })
    .expect("session setup") else { return };
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

/// Listing a directory's contents should work even under deny
/// (readdir is a directory operation, not a regular file open).
#[test]
fn readdir_allowed_under_deny() {
    let Some(s) = AgfsSession::new_with_config(Config {
        ask_default: Some(Perm::Deny),
        rules: BTreeMap::from([("/".into(), Perm::Deny)]),
        ..Default::default()
    })
    .expect("session setup") else { return };
    let entries: Vec<_> = fs::read_dir(s.mnt_path(""))
        .expect("readdir should succeed under deny")
        .collect();
    assert!(!entries.is_empty(), "directory should have entries");
}

// ── Base directory permissions are enforced ──────────────────────────

/// Creating a file inside a read-only base directory should fail because
/// agfs_permission delegates directory checks to the lower filesystem.
/// Uses agfs exec (which drops CAP_DAC_OVERRIDE) so base dir permissions
/// are enforced.
#[test]
fn create_in_readonly_base_dir_denied() {
    let Some(s) = AgfsSession::new_with_config(Config {
        permission: true,
        rules: BTreeMap::from([("/".into(), Perm::Allow)]),
        ..Default::default()
    })
    .expect("session setup") else { return };

    let dir = s.base_path("subdir");
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o555)).expect("chmod");

    // Run through agfs exec which drops caps — CAP_DAC_OVERRIDE would
    // otherwise bypass the directory permission check.
    let target = s.root.join("subdir/newfile.txt");
    let code = s
        .run_in_sandbox(&["sh", "-c", &format!("echo data > {}", target.display())])
        .unwrap();
    assert_ne!(code, 0, "create should fail in read-only base dir");

    fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).expect("restore");
}

/// mkdir inside a read-only base directory should fail.
#[test]
fn mkdir_in_readonly_base_dir_denied() {
    let Some(s) = AgfsSession::new_with_config(Config {
        permission: true,
        rules: BTreeMap::from([("/".into(), Perm::Allow)]),
        ..Default::default()
    })
    .expect("session setup") else { return };

    let dir = s.base_path("subdir");
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o555)).expect("chmod");

    let target = s.root.join("subdir/newdir");
    let code = s
        .run_in_sandbox(&["mkdir", &target.display().to_string()])
        .unwrap();
    assert_ne!(code, 0, "mkdir should fail in read-only base dir");

    fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).expect("restore");
}

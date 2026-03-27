use crate::helpers::AgfsSession;
use agfs::config::{Config, Perm};
use std::collections::BTreeMap;
use std::fs;

// ── Directory ops are now gated by agfs permission rules ──

/// mkdir should fail under deny.
#[test]
fn mkdir_denied_under_deny() {
    let s = AgfsSession::new_with_config(Config {
        ask_default: Some(Perm::Deny),
        rules: BTreeMap::from([("/".into(), Perm::Deny)]),
        ..Default::default()
    })
    .expect("session setup");

    let result = fs::create_dir(s.mnt_path("newdir"));
    assert!(result.is_err(), "mkdir should be denied");
}

/// unlink should fail under allow-ro (needs write on parent dir).
#[test]
fn unlink_denied_under_allow_ro() {
    let s = AgfsSession::new_with_config(Config {
        ask_default: Some(Perm::Deny),
        rules: BTreeMap::from([("/".into(), Perm::AllowRo)]),
        ..Default::default()
    })
    .expect("session setup");

    let result = fs::remove_file(s.mnt_path("hello.txt"));
    assert!(result.is_err(), "unlink should be denied under allow-ro");
}

/// symlink creation should fail under deny.
#[test]
fn symlink_denied_under_deny() {
    let s = AgfsSession::new_with_config(Config {
        ask_default: Some(Perm::Deny),
        rules: BTreeMap::from([("/".into(), Perm::Deny)]),
        ..Default::default()
    })
    .expect("session setup");

    let result = std::os::unix::fs::symlink("hello.txt", s.mnt_path("link.txt"));
    assert!(result.is_err(), "symlink should be denied");
}

/// rmdir should fail under deny.
#[test]
fn rmdir_denied_under_deny() {
    let s = AgfsSession::new_with_config(Config {
        ask_default: Some(Perm::Deny),
        rules: BTreeMap::from([("/".into(), Perm::Deny)]),
        ..Default::default()
    })
    .expect("session setup");

    let result = fs::remove_dir(s.mnt_path("subdir"));
    assert!(result.is_err(), "rmdir should be denied");
}

/// rename should fail under deny.
#[test]
fn rename_denied_under_deny() {
    let s = AgfsSession::new_with_config(Config {
        ask_default: Some(Perm::Deny),
        rules: BTreeMap::from([("/".into(), Perm::Deny)]),
        ..Default::default()
    })
    .expect("session setup");

    let result = fs::rename(s.mnt_path("hello.txt"), s.mnt_path("renamed.txt"));
    assert!(result.is_err(), "rename should be denied");
}

/// File creation should fail under deny (both dir op and open are gated).
#[test]
fn create_denied_under_deny() {
    let s = AgfsSession::new_with_config(Config {
        ask_default: Some(Perm::Deny),
        rules: BTreeMap::from([("/".into(), Perm::Deny)]),
        ..Default::default()
    })
    .expect("session setup");

    let result = fs::write(s.mnt_path("newfile.txt"), "data");
    assert!(result.is_err(), "write to new file should fail under deny");
    assert!(
        !s.mnt_path("newfile.txt").exists(),
        "file should not exist after denied create"
    );
}

/// Listing a directory's contents should work even under deny
/// (readdir is not a mutation, agfs_readdir doesn't check perm).
#[test]
fn readdir_allowed_under_deny() {
    let s = AgfsSession::new_with_config(Config {
        ask_default: Some(Perm::Deny),
        rules: BTreeMap::from([("/".into(), Perm::Deny)]),
        ..Default::default()
    })
    .expect("session setup");

    let entries: Vec<_> = fs::read_dir(s.mnt_path("."))
        .expect("readdir should succeed")
        .filter_map(|e| e.ok())
        .collect();
    assert!(
        !entries.is_empty(),
        "readdir should see base files even under deny"
    );
}

/// Stat should work even under deny (stat doesn't go through agfs_open).
#[test]
fn stat_allowed_under_deny() {
    let s = AgfsSession::new_with_config(Config {
        ask_default: Some(Perm::Deny),
        rules: BTreeMap::from([("/".into(), Perm::Deny)]),
        ..Default::default()
    })
    .expect("session setup");

    let meta = fs::metadata(s.mnt_path("hello.txt"));
    assert!(
        meta.is_ok(),
        "stat should succeed (no open needed): {:?}",
        meta.err()
    );
}

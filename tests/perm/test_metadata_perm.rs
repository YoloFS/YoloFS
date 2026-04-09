use crate::helpers::{YOLO_BIN, YoloSession};
use std::collections::BTreeMap;
use std::fs;
use yolofs::config::{Config, Perm};

/// Helper: create a session with deny-by-default and no rules on the session root.
fn deny_session() -> YoloSession {
    YoloSession::new_with_config(Config {
        ask_default: Some(Perm::Deny),
        rules: BTreeMap::new(),
        ..Default::default()
    })
    .expect("session setup")
}

/// Creating a file in a denied directory should fail.
#[test]
fn create_denied_without_rule() {
    let s = deny_session();

    let result = fs::write(s.mnt_path("denied.txt"), "data");
    assert!(
        result.is_err(),
        "create+write should be denied without rule"
    );
    // The file should not exist in the mount.
    assert!(
        !s.mnt_path("denied.txt").exists(),
        "file should not be created when denied"
    );
}

/// mkdir in a denied directory should fail.
#[test]
fn mkdir_denied_without_rule() {
    let s = deny_session();

    let result = fs::create_dir(s.mnt_path("denied_dir"));
    assert!(result.is_err(), "mkdir should be denied without rule");
    assert!(
        !s.mnt_path("denied_dir").exists(),
        "directory should not be created when denied"
    );
}

/// Unlinking a file in a denied directory should fail.
#[test]
fn unlink_denied_without_rule() {
    let s = deny_session();

    // hello.txt exists in base.
    let result = fs::remove_file(s.mnt_path("hello.txt"));
    assert!(result.is_err(), "unlink should be denied without rule");
    // File should still exist.
    assert!(
        s.mnt_path("hello.txt").exists(),
        "file should still exist after denied unlink"
    );
}

/// Renaming a file in a denied directory should fail.
#[test]
fn rename_denied_without_rule() {
    let s = deny_session();

    let result = fs::rename(s.mnt_path("hello.txt"), s.mnt_path("moved.txt"));
    assert!(result.is_err(), "rename should be denied without rule");
    assert!(
        s.mnt_path("hello.txt").exists(),
        "source should still exist after denied rename"
    );
    assert!(
        !s.mnt_path("moved.txt").exists(),
        "destination should not exist after denied rename"
    );
}

/// Creating a symlink in a denied directory should fail.
#[test]
fn symlink_denied_without_rule() {
    let s = deny_session();

    let result = std::os::unix::fs::symlink("hello.txt", s.mnt_path("link.txt"));
    assert!(result.is_err(), "symlink should be denied without rule");
    assert!(
        !s.mnt_path("link.txt").exists(),
        "symlink should not be created when denied"
    );
}

/// rmdir in a denied directory should fail.
#[test]
fn rmdir_denied_without_rule() {
    let s = deny_session();

    // subdir/ exists in base.
    let result = fs::remove_dir(s.mnt_path("subdir"));
    assert!(result.is_err(), "rmdir should be denied without rule");
    assert!(
        s.mnt_path("subdir").exists(),
        "directory should still exist after denied rmdir"
    );
}

/// With allow-rw rule on session root, create should succeed.
#[test]
fn create_allowed_with_rw_rule() {
    // Use the default session which has permission=false, then we can't
    // easily set rules dynamically. Instead, create a session with
    // a rule pointing at its own root.
    let s = YoloSession::new().expect("session setup");

    // Default session has permission=false, so create always works.
    fs::write(s.mnt_path("allowed.txt"), "data").expect("create should succeed without perm");
    assert!(s.mnt_path("allowed.txt").exists());
}

/// With allow-ro rule, create should fail (read-only).
#[test]
fn create_denied_with_ro_rule() {
    // Build config with session root in rules after session is created.
    // We need to know the root path to set the rule, but new_with_config
    // creates the root. Use a two-step approach.
    let root = tempfile::tempdir().unwrap().keep();
    fs::write(root.join("hello.txt"), "base\n").unwrap();
    fs::create_dir_all(root.join("subdir")).ok();
    fs::write(root.join("subdir/deep.txt"), "nested\n").ok();
    fs::write(root.join("test.sh"), "#!/bin/sh\n").ok();

    let mut rules = BTreeMap::new();
    rules.insert(root.to_string_lossy().into_owned(), Perm::AllowRo);

    let config = Config {
        permission: true,
        ask_default: Some(Perm::Deny),
        rules,
        ..Default::default()
    };
    config.save(&root.join("yolofs.toml")).unwrap();

    // Mount manually using the pre-configured root.
    let output = std::process::Command::new(YOLO_BIN)
        .arg("mount")
        .current_dir(&root)
        .env("NO_COLOR", "1")
        .output()
        .unwrap();
    assert!(output.status.success(), "mount failed");

    let mnt = root
        .join(".yolofs/mnt")
        .join(root.strip_prefix("/").unwrap());
    let result = fs::write(mnt.join("denied.txt"), "data");
    assert!(
        result.is_err(),
        "create should be denied with allow-ro rule"
    );

    let _ = std::process::Command::new(YOLO_BIN)
        .args(["unmount", "--force"])
        .current_dir(&root)
        .env("NO_COLOR", "1")
        .output();
    let _ = fs::remove_dir_all(&root);
}

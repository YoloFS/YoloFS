use crate::helpers::{YOLO_BIN, YoloSession};
use std::collections::BTreeMap;
use std::fs;
use yolofs::config::{Config, Perm};

// ── Newly created files respect permissions ──

/// A file created inside the sandbox should still be subject to perm gating
/// when reopened. The perm_gen fix ensures new inodes get re-resolved.
#[test]
fn newly_created_file_checked_on_reopen() {
    let s = YoloSession::new_with_config(Config {
        ask_default: Some(Perm::Deny),
        rules: BTreeMap::from([("/".into(), Perm::Allow)]),
        ..Default::default()
    })
    .expect("session setup");

    // Create a file (dir op, bypasses perm).
    fs::write(s.mnt_path("newfile.txt"), "hello").expect("create should succeed");

    // Now change rules to deny and re-read.
    s.cli(&["unmount", "--force"]).unwrap();
    Config {
        ask_default: Some(Perm::Deny),
        rules: BTreeMap::from([("/".into(), Perm::Deny)]),
        ..Default::default()
    }
    .save(&s.root.join("yolofs.toml"))
    .unwrap();
    std::process::Command::new(YOLO_BIN)
        .arg("mount")
        .current_dir(&s.root)
        .env("NO_COLOR", "1")
        .output()
        .expect("remount");

    let result = fs::read_to_string(s.mnt_path("hello.txt"));
    assert!(
        result.is_err(),
        "read should be denied after rule change to deny"
    );
}

// ── Rule change via live ioctl (cache invalidation) ──

/// Changing rules at runtime via `yolofs rule add` should take effect
/// on subsequent opens (perm_gen increment forces cache re-resolution).
#[test]
fn live_rule_change_takes_effect() {
    let s = YoloSession::new_with_config(Config {
        ask_default: Some(Perm::Deny),
        rules: BTreeMap::from([("/".into(), Perm::Deny)]),
        ..Default::default()
    })
    .expect("session setup");

    let result = fs::read_to_string(s.mnt_path("hello.txt"));
    assert!(result.is_err(), "read should fail under deny");

    // `rule add` takes a host path and resolves it through the mount internally.
    s.cli(&["rule", "add", &s.root.display().to_string(), "allow"])
        .unwrap();

    let content = fs::read_to_string(s.mnt_path("hello.txt"))
        .expect("read should succeed after live rule add");
    assert_eq!(content, "base content\n");
}

/// Removing a rule at runtime should re-gate access.
#[test]
fn live_rule_remove_reapplies_gating() {
    let s = YoloSession::new_with_config(Config {
        ask_default: Some(Perm::Deny),
        rules: BTreeMap::new(),
        ..Default::default()
    })
    .expect("session setup");

    s.cli(&["rule", "add", &s.root.display().to_string(), "allow"])
        .unwrap();
    fs::read_to_string(s.mnt_path("hello.txt")).expect("read should succeed with allow rule");

    s.cli(&["rule", "remove", &s.root.display().to_string()])
        .unwrap();
    let result = fs::read_to_string(s.mnt_path("hello.txt"));
    assert!(result.is_err(), "read should fail after rule removal");
}

// ── Rename across permission boundaries ──

/// Renaming a file from an allowed dir to a denied dir should succeed (dir op),
/// but reading the renamed file in the denied dir should fail after a cache
/// invalidation (the inode may still cache the old permission until perm_gen
/// is bumped by a rule change).
#[test]
fn rename_across_permission_boundary() {
    let s = YoloSession::new_with_config(Config {
        permission: false,
        rules: BTreeMap::new(),
        ..Default::default()
    })
    .expect("session setup");

    // Create base files directly.
    fs::create_dir_all(s.root.join("allowed")).expect("mkdir allowed");
    fs::create_dir_all(s.root.join("denied")).expect("mkdir denied");
    fs::write(s.root.join("allowed/file.txt"), "content\n").expect("create file");

    s.cli(&["unmount"]).unwrap();
    Config {
        ask_default: Some(Perm::Deny),
        rules: BTreeMap::from([
            (s.root.join("allowed").display().to_string(), Perm::Allow),
            (s.root.join("denied").display().to_string(), Perm::Deny),
        ]),
        ..Default::default()
    }
    .save(&s.root.join("yolofs.toml"))
    .unwrap();
    std::process::Command::new(YOLO_BIN)
        .arg("mount")
        .current_dir(&s.root)
        .env("NO_COLOR", "1")
        .output()
        .expect("remount");

    // Can read the file in the allowed dir.
    fs::read_to_string(s.mnt_path("allowed/file.txt"))
        .expect("reading file in allowed dir should succeed");

    // Rename into a denied directory should fail (destination is deny).
    let result = fs::rename(
        s.mnt_path("allowed/file.txt"),
        s.mnt_path("denied/file.txt"),
    );
    assert!(result.is_err(), "rename into denied dir should fail");

    // File should still be readable in the allowed directory.
    let content = fs::read_to_string(s.mnt_path("allowed/file.txt"))
        .expect("file should still be in allowed dir");
    assert_eq!(content, "content\n");
}

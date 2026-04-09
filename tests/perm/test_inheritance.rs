use crate::helpers::{YOLO_BIN, YoloSession};
use std::collections::BTreeMap;
use std::fs;
use yolofs::config::{Config, Perm};

// ── Rule inheritance (perm.c: yolo_resolve_perm walks dentry chain) ──

/// A more specific (child) rule should override a broader (parent) rule.
/// "/" = deny, but session root = allow → files in session are accessible.
#[test]
fn child_rule_overrides_parent() {
    let s = YoloSession::new_with_config(Config {
        permission: false,
        rules: BTreeMap::new(),
        ..Default::default()
    })
    .expect("session setup");

    // Unmount, write config with root-path-dependent rules, remount
    s.cli(&["unmount"]).unwrap();
    let root_path = s.root.display().to_string();
    Config {
        ask_default: Some(Perm::Deny),
        rules: BTreeMap::from([("/".into(), Perm::Deny), (root_path, Perm::Allow)]),
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

    let content = fs::read_to_string(s.mnt_path("hello.txt"))
        .expect("child allow should override parent deny");
    assert_eq!(content, "base content\n");
}

/// Rules resolve to the closest ancestor: / = deny, root/a/b = allow.
/// Files under a/b/c inherit allow; files at top level get denied.
#[test]
fn deep_nested_rules_closest_wins() {
    let s = YoloSession::new_with_config(Config {
        permission: false,
        rules: BTreeMap::new(),
        ..Default::default()
    })
    .expect("session setup");

    // Create base files directly (not through mount) so they survive remount.
    fs::create_dir_all(s.root.join("a/b/c")).expect("mkdir -p");
    fs::write(s.root.join("a/b/c/deep.txt"), "deep content\n").expect("create deep file");

    // Remount with tiered rules.
    s.cli(&["unmount"]).unwrap();
    Config {
        ask_default: Some(Perm::Deny),
        rules: BTreeMap::from([
            ("/".into(), Perm::Deny),
            (s.root.join("a/b").display().to_string(), Perm::Allow),
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

    let content = fs::read_to_string(s.mnt_path("a/b/c/deep.txt"))
        .expect("deep file should be readable via inherited allow");
    assert_eq!(content, "deep content\n");

    let result = fs::read_to_string(s.mnt_path("hello.txt"));
    assert!(
        result.is_err(),
        "top-level file should be denied with / = deny"
    );
}

/// Different directories can have different permission rules simultaneously.
#[test]
fn different_paths_different_rules() {
    let s = YoloSession::new_with_config(Config {
        permission: false,
        rules: BTreeMap::new(),
        ..Default::default()
    })
    .expect("session setup");

    // Create base files directly.
    fs::create_dir_all(s.root.join("readonly")).expect("mkdir readonly");
    fs::write(s.root.join("readonly/data.txt"), "ro content\n").expect("create ro file");
    fs::create_dir_all(s.root.join("writable")).expect("mkdir writable");
    fs::write(s.root.join("writable/data.txt"), "rw content\n").expect("create rw file");

    s.cli(&["unmount"]).unwrap();
    Config {
        ask_default: Some(Perm::Deny),
        rules: BTreeMap::from([
            (s.root.join("readonly").display().to_string(), Perm::Ro),
            (s.root.join("writable").display().to_string(), Perm::Allow),
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

    // Read should work in both.
    fs::read_to_string(s.mnt_path("readonly/data.txt")).expect("read should succeed in ro dir");
    fs::read_to_string(s.mnt_path("writable/data.txt")).expect("read should succeed in allow dir");

    // Write should fail in readonly, succeed in writable.
    let result = fs::write(s.mnt_path("readonly/data.txt"), "modified\n");
    assert!(result.is_err(), "write should fail in ro dir");
    fs::write(s.mnt_path("writable/data.txt"), "modified\n")
        .expect("write should succeed in allow dir");
}

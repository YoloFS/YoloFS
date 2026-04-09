use crate::helpers::YoloSession;
use std::collections::BTreeMap;
use std::fs;
use yolofs::config::{Config, Perm};

// ── ro tests (perm.c: yolo_check_perm, inode.c: yolo_permission) ──

/// ro should permit reads but deny writes.
#[test]
fn ro_permits_read_denies_write() {
    let s = YoloSession::new_with_config(Config {
        ask_default: Some(Perm::Deny),
        rules: BTreeMap::from([("/".into(), Perm::Ro)]),
        ..Default::default()
    })
    .expect("session setup");

    // Read should succeed
    let content =
        fs::read_to_string(s.mnt_path("hello.txt")).expect("read should succeed with ro");
    assert_eq!(content, "base content\n");

    // Write should fail
    let result = fs::write(s.mnt_path("hello.txt"), "modified\n");
    assert!(result.is_err(), "write should be denied with ro rule");
}

// ── allow tests (perm.c: yolo_check_perm, inode.c: yolo_permission) ──

/// allow should permit reads.
#[test]
fn allow_permits_read() {
    let s = YoloSession::new_with_config(Config {
        ask_default: Some(Perm::Deny),
        rules: BTreeMap::from([("/".into(), Perm::Allow)]),
        ..Default::default()
    })
    .expect("session setup");

    let content =
        fs::read_to_string(s.mnt_path("hello.txt")).expect("read should succeed with allow");
    assert_eq!(content, "base content\n");
}

/// allow should permit writes.
#[test]
fn allow_permits_write() {
    let s = YoloSession::new_with_config(Config {
        ask_default: Some(Perm::Deny),
        rules: BTreeMap::from([("/".into(), Perm::Allow)]),
        ..Default::default()
    })
    .expect("session setup");

    fs::write(s.mnt_path("hello.txt"), "modified\n").expect("write should succeed with allow");
}

/// allow should permit exec.
#[test]
fn allow_permits_exec() {
    let s = YoloSession::new_with_config(Config {
        ask_default: Some(Perm::Deny),
        rules: BTreeMap::from([("/".into(), Perm::Allow)]),
        ..Default::default()
    })
    .expect("session setup");

    let output = std::process::Command::new(s.mnt_path("test.sh"))
        .output()
        .expect("should be able to spawn executable");
    assert!(output.status.success(), "exec should succeed with allow");
}

// ── ro tests (perm.c: yolo_check_perm, inode.c: yolo_permission) ──

/// ro should permit exec.
#[test]
fn ro_permits_exec() {
    let s = YoloSession::new_with_config(Config {
        ask_default: Some(Perm::Deny),
        rules: BTreeMap::from([("/".into(), Perm::Ro)]),
        ..Default::default()
    })
    .expect("session setup");

    let output = std::process::Command::new(s.mnt_path("test.sh"))
        .output()
        .expect("should be able to spawn executable");
    assert!(output.status.success(), "exec should succeed with ro");
}

// ── deny (all blocked) ──

/// deny should block writes (not just reads).
#[test]
fn deny_blocks_write() {
    let s = YoloSession::new_with_config(Config {
        ask_default: Some(Perm::Allow),
        rules: BTreeMap::from([("/".into(), Perm::Deny)]),
        ..Default::default()
    })
    .expect("session setup");

    let result = fs::write(s.mnt_path("hello.txt"), "modified\n");
    assert!(result.is_err(), "write should be denied with deny rule");
}

/// deny should block exec.
#[test]
fn deny_blocks_exec() {
    let s = YoloSession::new_with_config(Config {
        ask_default: Some(Perm::Allow),
        rules: BTreeMap::from([("/".into(), Perm::Deny)]),
        ..Default::default()
    })
    .expect("session setup");

    let result = std::process::Command::new(s.mnt_path("test.sh")).output();
    assert!(
        result.is_err() || !result.unwrap().status.success(),
        "exec should be denied with deny rule"
    );
}

use crate::helpers::YoloSession;
use std::collections::BTreeMap;
use std::fs;
use yolofs::config::Config;
use yolofs::perm::Perm;

// ── ask_default variants (perm.c: yolo_ask_userspace no-daemon path) ──

/// ask_default=ro: read OK, write denied.
#[test]
fn ask_default_ro_read_ok_write_denied() {
    let s = YoloSession::new_with_config(Config {
        ask_default: Some(Perm::Read),
        rules: BTreeMap::new(),
        ..Default::default()
    })
    .expect("session setup");

    let content = fs::read_to_string(s.mnt_path("hello.txt"))
        .expect("read should succeed with ask_default=ro");
    assert_eq!(content, "base content\n");

    let result = fs::write(s.mnt_path("hello.txt"), "modified\n");
    assert!(
        result.is_err(),
        "write should be denied with ask_default=ro"
    );
}

/// ask_default=allow: read OK, write OK.
#[test]
fn ask_default_allow_read_ok_write_ok() {
    let s = YoloSession::new_with_config(Config {
        ask_default: Some(Perm::Allow),
        rules: BTreeMap::new(),
        ..Default::default()
    })
    .expect("session setup");

    let content = fs::read_to_string(s.mnt_path("hello.txt"))
        .expect("read should succeed with ask_default=allow");
    assert_eq!(content, "base content\n");

    fs::write(s.mnt_path("hello.txt"), "modified\n")
        .expect("write should succeed with ask_default=allow");
}

// ── ask_timeout applies default when no daemon ──

/// With ask_timeout set and no daemon, the ask should time out and
/// apply the ask_default.
#[test]
fn ask_timeout_applies_default() {
    let s = YoloSession::new_with_config(Config {
        ask_timeout: Some(1),
        ask_default: Some(Perm::Deny),
        rules: BTreeMap::new(),
        ..Default::default()
    })
    .expect("session setup");

    // No daemon running — ask times out, applies ask_default=deny.
    let result = fs::read_to_string(s.mnt_path("hello.txt"));
    assert!(
        result.is_err(),
        "read should be denied when ask times out with ask_default=deny"
    );
}

/// With ask_timeout and ask_default=allow, timed out ask should allow.
#[test]
fn ask_timeout_applies_allow_default() {
    let s = YoloSession::new_with_config(Config {
        ask_timeout: Some(1),
        ask_default: Some(Perm::Allow),
        rules: BTreeMap::new(),
        ..Default::default()
    })
    .expect("session setup");

    let content = fs::read_to_string(s.mnt_path("hello.txt"))
        .expect("read should succeed when ask times out with ask_default=allow");
    assert_eq!(content, "base content\n");
}

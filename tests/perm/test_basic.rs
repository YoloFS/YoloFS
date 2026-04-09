use crate::helpers::YoloSession;
use std::collections::BTreeMap;
use std::fs;
use yolofs::config::{Config, Perm};

/// With no daemon and ask_default=deny, reading an unruled file should fail.
#[test]
fn no_daemon_denies_by_default() {
    let s = YoloSession::new_with_config(Config {
        ask_default: Some(Perm::Deny),
        rules: BTreeMap::new(),
        ..Default::default()
    })
    .expect("session setup");

    let result = fs::read_to_string(s.mnt_path("hello.txt"));
    assert!(
        result.is_err(),
        "read should be denied without daemon or rule"
    );
}

/// With no daemon and ask_default=allow, reading an unruled file should succeed.
#[test]
fn no_daemon_allows_when_configured() {
    let s = YoloSession::new_with_config(Config {
        ask_default: Some(Perm::Allow),
        rules: BTreeMap::new(),
        ..Default::default()
    })
    .expect("session setup");

    let content = fs::read_to_string(s.mnt_path("hello.txt"))
        .expect("read should succeed with ask_default=allow");
    assert_eq!(content, "base content\n");
}

/// An explicit allow rule should bypass the ask mechanism entirely.
#[test]
fn explicit_rule_bypasses_ask() {
    let s = YoloSession::new_with_config(Config {
        ask_default: Some(Perm::Deny),
        rules: BTreeMap::from([("/".into(), Perm::Allow)]),
        ..Default::default()
    })
    .expect("session setup");

    let content = fs::read_to_string(s.mnt_path("hello.txt"))
        .expect("read should succeed with explicit allow rule");
    assert_eq!(content, "base content\n");
}

/// An explicit deny rule should block access even with permission=true.
#[test]
fn deny_rule_blocks_access() {
    let s = YoloSession::new_with_config(Config {
        ask_default: Some(Perm::Allow),
        rules: BTreeMap::from([("/".into(), Perm::Deny)]),
        ..Default::default()
    })
    .expect("session setup");

    let result = fs::read_to_string(s.mnt_path("hello.txt"));
    assert!(
        result.is_err(),
        "read should be denied with explicit deny rule"
    );
}

/// With permission=false, everything is allowed regardless of rules.
#[test]
fn permission_disabled_allows_everything() {
    let s = YoloSession::new_with_config(Config {
        permission: false,
        rules: BTreeMap::from([("/".into(), Perm::Deny)]),
        ..Default::default()
    })
    .expect("session setup");

    let content = fs::read_to_string(s.mnt_path("hello.txt"))
        .expect("read should succeed with permission=false even with deny rule");
    assert_eq!(content, "base content\n");
}

use crate::helpers::AgfsSession;
use agfs::config::{Config, Perm};
use std::collections::BTreeMap;
use std::fs;

// ── ask_default variants (perm.c: agfs_ask_userspace no-daemon path) ──

/// ask_default=allow-ro: read OK, write denied.
#[test]
fn ask_default_allow_ro() {
    let s = AgfsSession::new_with_config(Config {
        ask_default: Some(Perm::AllowRo),
        rules: BTreeMap::new(),
        ..Default::default()
    })
    .expect("session setup");

    let content = fs::read_to_string(s.mnt_path("hello.txt"))
        .expect("read should succeed with ask_default=allow-ro");
    assert_eq!(content, "base content\n");

    let result = fs::write(s.mnt_path("hello.txt"), "modified\n");
    assert!(
        result.is_err(),
        "write should be denied with ask_default=allow-ro"
    );
}

/// ask_default=allow-rw: read OK, write OK.
#[test]
fn ask_default_allow_rw() {
    let s = AgfsSession::new_with_config(Config {
        ask_default: Some(Perm::AllowRw),
        rules: BTreeMap::new(),
        ..Default::default()
    })
    .expect("session setup");

    let content = fs::read_to_string(s.mnt_path("hello.txt"))
        .expect("read should succeed with ask_default=allow-rw");
    assert_eq!(content, "base content\n");

    fs::write(s.mnt_path("hello.txt"), "modified\n")
        .expect("write should succeed with ask_default=allow-rw");
}

/// ask_default=allow-rx: read OK, write denied.
#[test]
fn ask_default_allow_rx() {
    let s = AgfsSession::new_with_config(Config {
        ask_default: Some(Perm::AllowRx),
        rules: BTreeMap::new(),
        ..Default::default()
    })
    .expect("session setup");

    let content = fs::read_to_string(s.mnt_path("hello.txt"))
        .expect("read should succeed with ask_default=allow-rx");
    assert_eq!(content, "base content\n");

    let result = fs::write(s.mnt_path("hello.txt"), "modified\n");
    assert!(
        result.is_err(),
        "write should be denied with ask_default=allow-rx"
    );
}

// ── ask_timeout applies default when no daemon ──

/// With ask_timeout set and no daemon, the ask should time out and
/// apply the ask_default.
#[test]
fn ask_timeout_applies_default() {
    let s = AgfsSession::new_with_config(Config {
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
    let s = AgfsSession::new_with_config(Config {
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

use crate::helpers::YoloSession;
use std::collections::BTreeMap;
use std::fs;
use yolofs::config::{Config, Perm};

// ── O_TRUNC treated as write ──

/// O_TRUNC counts as a write operation; allow-ro should deny it.
#[test]
fn truncate_denied_on_allow_ro() {
    let s = YoloSession::new_with_config(Config {
        ask_default: Some(Perm::Deny),
        rules: BTreeMap::from([("/".into(), Perm::AllowRo)]),
        ..Default::default()
    })
    .expect("session setup");

    let result = fs::OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(s.mnt_path("hello.txt"));
    assert!(result.is_err(), "O_TRUNC should be denied with allow-ro");
}

/// O_APPEND counts as a write operation; allow-ro should deny it.
#[test]
fn append_denied_on_allow_ro() {
    let s = YoloSession::new_with_config(Config {
        ask_default: Some(Perm::Deny),
        rules: BTreeMap::from([("/".into(), Perm::AllowRo)]),
        ..Default::default()
    })
    .expect("session setup");

    let result = fs::OpenOptions::new()
        .append(true)
        .open(s.mnt_path("hello.txt"));
    assert!(result.is_err(), "O_APPEND should be denied with allow-ro");
}

/// O_RDWR counts as a write; allow-ro should deny it.
#[test]
fn rdwr_denied_on_allow_ro() {
    let s = YoloSession::new_with_config(Config {
        ask_default: Some(Perm::Deny),
        rules: BTreeMap::from([("/".into(), Perm::AllowRo)]),
        ..Default::default()
    })
    .expect("session setup");

    let result = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(s.mnt_path("hello.txt"));
    assert!(result.is_err(), "O_RDWR should be denied with allow-ro");
}

// ── allow-rw permits truncate/append ──

/// allow-rw should permit O_TRUNC.
#[test]
fn truncate_allowed_on_allow_rw() {
    let s = YoloSession::new_with_config(Config {
        ask_default: Some(Perm::Deny),
        rules: BTreeMap::from([("/".into(), Perm::AllowRw)]),
        ..Default::default()
    })
    .expect("session setup");

    fs::OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(s.mnt_path("hello.txt"))
        .expect("O_TRUNC should succeed with allow-rw");
}

// ── allow-rx ──

/// allow-rx should permit read + exec but deny O_TRUNC.
#[test]
fn allow_rx_denies_truncate() {
    let s = YoloSession::new_with_config(Config {
        ask_default: Some(Perm::Deny),
        rules: BTreeMap::from([("/".into(), Perm::AllowRx)]),
        ..Default::default()
    })
    .expect("session setup");

    let result = fs::OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(s.mnt_path("hello.txt"));
    assert!(result.is_err(), "O_TRUNC should be denied with allow-rx");
}

/// allow-rx should deny O_APPEND.
#[test]
fn allow_rx_denies_append() {
    let s = YoloSession::new_with_config(Config {
        ask_default: Some(Perm::Deny),
        rules: BTreeMap::from([("/".into(), Perm::AllowRx)]),
        ..Default::default()
    })
    .expect("session setup");

    let result = fs::OpenOptions::new()
        .append(true)
        .open(s.mnt_path("hello.txt"));
    assert!(result.is_err(), "O_APPEND should be denied with allow-rx");
}

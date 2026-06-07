use crate::helpers::YoloSession;
use std::collections::BTreeMap;
use std::fs;
use yolofs::config::Config;
use yolofs::perm::Perm;

// ── O_TRUNC treated as write ──

/// O_TRUNC counts as a write operation; ro should deny it.
#[test]
fn truncate_denied_on_ro() {
    let s = YoloSession::new_with_config(Config {
        rules: BTreeMap::from([("/".into(), Perm::ReadOnly)]),
        ..Default::default()
    })
    .expect("session setup");

    let result = fs::OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(s.mnt_path("hello.txt"));
    assert!(result.is_err(), "O_TRUNC should be denied with ro");
}

/// O_APPEND counts as a write operation; ro should deny it.
#[test]
fn append_denied_on_ro() {
    let s = YoloSession::new_with_config(Config {
        rules: BTreeMap::from([("/".into(), Perm::ReadOnly)]),
        ..Default::default()
    })
    .expect("session setup");

    let result = fs::OpenOptions::new()
        .append(true)
        .open(s.mnt_path("hello.txt"));
    assert!(result.is_err(), "O_APPEND should be denied with ro");
}

/// O_RDWR counts as a write; ro should deny it.
#[test]
fn rdwr_denied_on_ro() {
    let s = YoloSession::new_with_config(Config {
        rules: BTreeMap::from([("/".into(), Perm::ReadOnly)]),
        ..Default::default()
    })
    .expect("session setup");

    let result = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(s.mnt_path("hello.txt"));
    assert!(result.is_err(), "O_RDWR should be denied with ro");
}

// ── allow permits truncate/append ──

/// allow should permit O_TRUNC.
#[test]
fn truncate_allowed_on_allow() {
    let s = YoloSession::new_with_config(Config {
        rules: BTreeMap::from([("/".into(), Perm::Allow)]),
        ..Default::default()
    })
    .expect("session setup");

    fs::OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(s.mnt_path("hello.txt"))
        .expect("O_TRUNC should succeed with allow");
}

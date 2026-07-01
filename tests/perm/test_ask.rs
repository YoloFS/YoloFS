use crate::helpers::YoloSession;
use std::collections::BTreeMap;
use std::fs;
use yolofs::config::Config;

// ── Unanswered ask → deny (perm.c: yolo_ask_userspace no-daemon path) ──

/// With no daemon connected, an `ask` path (the default for unruled paths) is
/// denied for both read and write — an unanswered ask is a deny.
#[test]
fn no_daemon_denies_ask() {
    let s = YoloSession::new_with_config(Config {
        rules: BTreeMap::new(),
        ..Default::default()
    })
    .expect("session setup");

    assert!(
        fs::read_to_string(s.mnt_path("hello.txt")).is_err(),
        "read should be denied with no daemon"
    );
    assert!(
        fs::write(s.mnt_path("hello.txt"), "modified\n").is_err(),
        "write should be denied with no daemon"
    );
}

/// With no daemon answering, an ask waits for `prompt_timeout` (here 100ms)
/// and is then denied.
#[test]
fn prompt_timeout_no_daemon_denies() {
    let s = YoloSession::new_with_config(Config {
        prompt_timeout: Some(0.1),
        rules: BTreeMap::new(),
        ..Default::default()
    })
    .expect("session setup");

    assert!(
        fs::read_to_string(s.mnt_path("hello.txt")).is_err(),
        "read should be denied when no daemon answers"
    );
}

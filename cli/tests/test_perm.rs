use crate::helpers::AgfsSession;
use std::fs;

/// With no daemon and ask_default=deny, reading an unruled file should fail.
#[test]
fn no_daemon_denies_by_default() {
    let config = format!(
        "[mount]\nask_default = \"deny\"\n\n[rules]\n"
    );
    let s = AgfsSession::new_with_config(&config).expect("session setup");

    let result = fs::read_to_string(s.mnt_path("hello.txt"));
    assert!(result.is_err(), "read should be denied without daemon or rule");
}

/// With no daemon and ask_default=allow, reading an unruled file should succeed.
#[test]
fn no_daemon_allows_when_configured() {
    let config = format!(
        "[mount]\nask_default = \"allow\"\n\n[rules]\n"
    );
    let s = AgfsSession::new_with_config(&config).expect("session setup");

    let content = fs::read_to_string(s.mnt_path("hello.txt"))
        .expect("read should succeed with ask_default=allow");
    assert_eq!(content, "base content\n");
}

/// An explicit allow rule should bypass the ask mechanism entirely.
#[test]
fn explicit_rule_bypasses_ask() {
    let s = AgfsSession::new_with_config(&format!(
        "[mount]\nask_default = \"deny\"\n\n[rules]\n\"{}\" = \"allow\"\n",
        // Rule the session root so hello.txt is covered
        "/"
    )).expect("session setup");

    let content = fs::read_to_string(s.mnt_path("hello.txt"))
        .expect("read should succeed with explicit allow rule");
    assert_eq!(content, "base content\n");
}

/// An explicit deny rule should block access even with noperm=false.
#[test]
fn deny_rule_blocks_access() {
    let s = AgfsSession::new_with_config(&format!(
        "[mount]\nask_default = \"allow\"\n\n[rules]\n\"{}\" = \"deny\"\n",
        "/"
    )).expect("session setup");

    let result = fs::read_to_string(s.mnt_path("hello.txt"));
    assert!(result.is_err(), "read should be denied with explicit deny rule");
}

/// With noperm=true, everything is allowed regardless of rules.
#[test]
fn noperm_allows_everything() {
    let s = AgfsSession::new_with_config(
        "[mount]\nnoperm = true\n\n[rules]\n\"/\" = \"deny\"\n"
    ).expect("session setup");

    let content = fs::read_to_string(s.mnt_path("hello.txt"))
        .expect("read should succeed with noperm=true even with deny rule");
    assert_eq!(content, "base content\n");
}

/// allow-ro should permit reads but deny writes.
#[test]
fn allow_ro_permits_read_denies_write() {
    let s = AgfsSession::new_with_config(&format!(
        "[mount]\nask_default = \"deny\"\n\n[rules]\n\"{}\" = \"allow-ro\"\n",
        "/"
    )).expect("session setup");

    // Read should succeed
    let content = fs::read_to_string(s.mnt_path("hello.txt"))
        .expect("read should succeed with allow-ro");
    assert_eq!(content, "base content\n");

    // Write should fail
    let result = fs::write(s.mnt_path("hello.txt"), "modified\n");
    assert!(result.is_err(), "write should be denied with allow-ro rule");
}


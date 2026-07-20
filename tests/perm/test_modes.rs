use crate::helpers::YoloSession;
use std::collections::BTreeMap;
use std::fs;
use yolofs::config::Config;
use yolofs::perm::Perm;

// ── ro tests (perm.c: yolo_perm_check, inode.c: yolo_permission) ──

/// ro should permit reads but deny writes.
#[test]
fn ro_permits_read_denies_write() {
    let s = YoloSession::new_with_config(Config {
        rules: BTreeMap::from([("/".into(), Perm::ReadOnly)]),
        ..Default::default()
    })
    .expect("session setup");

    // Read should succeed
    let content = fs::read_to_string(s.mnt_path("hello.txt")).expect("read should succeed with ro");
    assert_eq!(content, "base content\n");

    // Write should fail
    let result = fs::write(s.mnt_path("hello.txt"), "modified\n");
    assert!(result.is_err(), "write should be denied with ro rule");
}

/// Under `read-only`, `open(O_WRONLY)` is denied by the `yolo_open` gate, but
/// `access(2)`/`faccessat(2)` does not reflect the yolo access policy:
/// `yolo_permission` no longer decides regular-file access (a regular file
/// passes there unconditionally, since writes are COW'd), so `access(W_OK)`
/// returns 0 even though writing through `open` is blocked. `open()` is the
/// real gate.
#[test]
fn ro_access_syscall_does_not_reflect_policy() {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let s = YoloSession::new_with_config(Config {
        rules: BTreeMap::from([("/".into(), Perm::ReadOnly)]),
        ..Default::default()
    })
    .expect("session setup");

    let path = s.mnt_path("hello.txt");
    let cpath = CString::new(path.as_os_str().as_bytes()).unwrap();

    // R_OK: readable via access() (and via open, tested above).
    assert_eq!(
        unsafe { libc::access(cpath.as_ptr(), libc::R_OK) },
        0,
        "access(R_OK) should succeed under read-only"
    );
    // W_OK: a regular file passes yolo_permission unconditionally, so access()
    // reports writable and does NOT reflect the read-only yolo policy.
    assert_eq!(
        unsafe { libc::access(cpath.as_ptr(), libc::W_OK) },
        0,
        "access(W_OK) should not reflect the read-only rule"
    );
    // But an actual write open is still gated and denied.
    assert!(
        fs::OpenOptions::new().write(true).open(&path).is_err(),
        "open(O_WRONLY) must still be denied under read-only"
    );
}

// ── allow tests (perm.c: yolo_perm_check, inode.c: yolo_permission) ──

/// allow should permit reads.
#[test]
fn allow_permits_read() {
    let s = YoloSession::new_with_config(Config {
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
        rules: BTreeMap::from([("/".into(), Perm::Allow)]),
        ..Default::default()
    })
    .expect("session setup");

    let output = std::process::Command::new(s.mnt_path("test.sh"))
        .output()
        .expect("should be able to spawn executable");
    assert!(output.status.success(), "exec should succeed with allow");
}

// ── ro tests (perm.c: yolo_perm_check, inode.c: yolo_permission) ──

/// ro should permit exec.
#[test]
fn ro_permits_exec() {
    let s = YoloSession::new_with_config(Config {
        rules: BTreeMap::from([("/".into(), Perm::ReadOnly)]),
        ..Default::default()
    })
    .expect("session setup");

    let output = std::process::Command::new(s.mnt_path("test.sh"))
        .output()
        .expect("should be able to spawn executable");
    assert!(output.status.success(), "exec should succeed with ro");
}

// ── write-ask tests ─────────────────────────────────────────────────

/// write-ask should permit reads without prompting.
#[test]
fn write_ask_permits_read() {
    let s = YoloSession::new_with_config(Config {
        rules: BTreeMap::from([("/".into(), Perm::WriteAsk)]),
        ..Default::default()
    })
    .expect("session setup");

    let content =
        fs::read_to_string(s.mnt_path("hello.txt")).expect("read should succeed with write-ask");
    assert_eq!(content, "base content\n");
}

/// write-ask should ask for writes; with no daemon, unanswered asks deny.
#[test]
fn write_ask_denies_write_without_daemon() {
    let s = YoloSession::new_with_config(Config {
        rules: BTreeMap::from([("/".into(), Perm::WriteAsk)]),
        ..Default::default()
    })
    .expect("session setup");

    let result = fs::write(s.mnt_path("hello.txt"), "modified\n");
    assert!(
        result.is_err(),
        "write should be denied when write-ask has no daemon"
    );
}

// ── deny (all blocked) ──

/// deny should block writes (not just reads).
#[test]
fn deny_blocks_write() {
    let s = YoloSession::new_with_config(Config {
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

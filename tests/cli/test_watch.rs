use crate::helpers::{YOLO_BIN, YoloSession};
use std::collections::BTreeMap;
use std::io::Write;
use std::process::{Command, Stdio};
use std::time::Duration;
use yolofs::config::{Config, Perm};

/// `yolofs watch --allow-all` should answer every ask with allow, so
/// `touch a` inside `yolofs exec` must succeed.
///
/// Regression: even with the daemon running and --allow-all set, the
/// kernel never delivers the ask for the new file and the touch fails.
#[test]
fn watch_allow_all_daemon_allows_file_creation_inside_exec() {
    let s = YoloSession::new_with_config(Config {
        ask_default: Some(Perm::Ask),
        rules: BTreeMap::new(),
        ..Default::default()
    })
    .expect("session setup");

    // Start the daemon before exec so it is already blocked in ioctl read
    // when the kernel raises the ask for the new file.
    let mut watch = std::process::Command::new(YOLO_BIN)
        .args(["watch", "--allow-all"])
        .current_dir(&s.root)
        .env("NO_COLOR", "1")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawning yolofs watch --allow-all");

    // Give the daemon time to open the ioctl fd and block on the first read.
    std::thread::sleep(Duration::from_millis(200));

    // touch creates a new file — the kernel must ask the daemon and receive
    // an allow response for this to succeed.
    let code = s.run_in_sandbox(&["touch", "a"]).unwrap_or(1);

    watch.kill().ok();
    let output = watch.wait_with_output().expect("collecting watch output");
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();

    // The daemon must have logged an ask whose path ends with /a.
    let expected_path = format!("{}/a", s.root.display());
    assert!(
        stderr.contains(&expected_path),
        "watch daemon should have received an ask for {expected_path}, got:\n{stderr}"
    );

    assert_eq!(
        code, 0,
        "touch a should succeed when watch --allow-all is running"
    );
}

/// Starting a second watch while one is already running should fail
/// with a clear "already running" message (kernel returns EBUSY).
#[test]
fn second_watch_reports_already_running() {
    let s = YoloSession::new_with_config(Config {
        ask_default: Some(Perm::Deny),
        rules: BTreeMap::new(),
        ..Default::default()
    })
    .expect("session setup");

    // Start first watch in background.
    let mut watch1 = Command::new(YOLO_BIN)
        .arg("watch")
        .current_dir(&s.root)
        .env("NO_COLOR", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn first watch");

    // Give it time to register with the kernel (ensure_ctl sets has_daemon).
    std::thread::sleep(Duration::from_millis(300));

    // Second watch should fail with "already running".
    let (ok, _, stderr) = s.cli_output(&["watch"]).unwrap();
    assert!(!ok, "second watch should fail");
    assert!(
        stderr.contains("already running"),
        "error should mention 'already running': {stderr}"
    );

    // Clean up.
    let _ = Command::new("kill").arg(watch1.id().to_string()).status();
    let _ = watch1.wait();
}

// ── Interactive daemon tests (PTY-free: pipe stdin/stderr) ──────────

/// Helper: mount with "/" = Allow (so dir traversal never triggers an ask),
/// then live-add an Ask rule for a single file so only that file's open()
/// goes through the interactive daemon path.
fn session_with_ask_file() -> (YoloSession, String) {
    let s = YoloSession::new_with_config(Config {
        ask_default: Some(Perm::Deny),
        rules: BTreeMap::from([("/".into(), Perm::Allow)]),
        ..Default::default()
    })
    .expect("session setup");

    let file_path = "hello.txt".to_string();
    s.cli(&["rule", "add", &file_path, "ask"]).unwrap();
    (s, file_path)
}

/// Interactive `yolofs watch` — daemon reads "a\n" from piped stdin and
/// responds Allow.  A subsequent read through the mount should succeed.
#[test]
fn interactive_watch_allow_permits_read() {
    let (s, _path) = session_with_ask_file();

    let mut watch = Command::new(YOLO_BIN)
        .args(["watch"])
        .current_dir(&s.root)
        .env("NO_COLOR", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn interactive watch");

    std::thread::sleep(Duration::from_millis(200));

    // Pre-fill stdin with "a" (allow).
    watch.stdin.as_mut().unwrap().write_all(b"a\n").unwrap();

    let content = std::fs::read_to_string(s.mnt_path("hello.txt"));

    watch.kill().ok();
    let output = watch.wait_with_output().unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();

    assert!(
        stderr.contains("[ask]"),
        "daemon should have prompted: {stderr}"
    );
    assert!(
        stderr.contains("→ allow"),
        "daemon should log allow decision: {stderr}"
    );

    let content = content.expect("read should succeed after interactive 'allow'");
    assert_eq!(content, "base content\n");
}

/// Interactive `yolofs watch` — daemon reads "d\n" from piped stdin and
/// responds Deny.  A subsequent read through the mount should fail.
#[test]
fn interactive_watch_deny_blocks_read() {
    let (s, _path) = session_with_ask_file();

    let mut watch = Command::new(YOLO_BIN)
        .args(["watch"])
        .current_dir(&s.root)
        .env("NO_COLOR", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn interactive watch");

    std::thread::sleep(Duration::from_millis(200));

    // Pre-fill stdin with "d" (deny).
    watch.stdin.as_mut().unwrap().write_all(b"d\n").unwrap();

    let result = std::fs::read_to_string(s.mnt_path("hello.txt"));

    watch.kill().ok();
    let output = watch.wait_with_output().unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();

    assert!(
        stderr.contains("[ask]"),
        "daemon should have prompted: {stderr}"
    );
    assert!(
        stderr.contains("→ deny"),
        "daemon should log deny decision: {stderr}"
    );
    assert!(result.is_err(), "read should fail after interactive 'deny'");
}

/// Interactive `yolofs watch` — daemon reads "ro\n" → AllowRo.
/// Read succeeds, but write is denied.
#[test]
fn interactive_watch_allow_ro_permits_read_denies_write() {
    let (s, _path) = session_with_ask_file();

    // Pre-fill two responses: "ro\n" for the read open, "ro\n" for the write open.
    let mut watch = Command::new(YOLO_BIN)
        .args(["watch"])
        .current_dir(&s.root)
        .env("NO_COLOR", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn interactive watch");

    std::thread::sleep(Duration::from_millis(200));

    watch
        .stdin
        .as_mut()
        .unwrap()
        .write_all(b"ro\nro\n")
        .unwrap();

    // Read should succeed (AllowRo permits reads).
    let content = std::fs::read_to_string(s.mnt_path("hello.txt"))
        .expect("read should succeed with allow-ro");
    assert_eq!(content, "base content\n");

    // Write should fail (AllowRo denies writes).
    let result = std::fs::write(s.mnt_path("hello.txt"), "overwritten\n");
    assert!(result.is_err(), "write should fail with allow-ro");

    watch.kill().ok();
    let _ = watch.wait();
}

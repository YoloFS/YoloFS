use crate::helpers::{YOLO_BIN, YoloSession};
use std::collections::BTreeMap;
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::process::{Command, Stdio};
use std::time::Duration;
use yolofs::config::Config;
use yolofs::ioctl;
use yolofs::journal::{Journal, Note, Op, Record};
use yolofs::perm::{Decision, Perm};

/// `yolofs watch --allow-all` should answer every ask with allow, so
/// `touch a` inside `yolofs exec` must succeed.
///
/// Regression: even with the daemon running and --allow-all set, the
/// kernel never delivers the ask for the new file and the touch fails.
#[test]
fn watch_allow_all_daemon_allows_file_creation_inside_exec() {
    let s = YoloSession::new_with_config(Config {
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
    let code = s.run_in_yolofs(&["touch", "a"]).unwrap_or(1);

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

/// `write-ask` allows reads but asks on each write. An approved write must not
/// cache `allow` over the inherited `write-ask` rule, or later writes would stop
/// prompting.
#[test]
fn watch_allow_all_answers_each_write_ask() {
    let s = YoloSession::new_with_config(Config {
        rules: BTreeMap::from([("/".into(), Perm::WriteAsk)]),
        ..Default::default()
    })
    .expect("session setup");

    let mut watch = std::process::Command::new(YOLO_BIN)
        .args(["watch", "--allow-all"])
        .current_dir(&s.root)
        .env("NO_COLOR", "1")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawning yolofs watch --allow-all");

    std::thread::sleep(Duration::from_millis(200));

    let code = s
        .run_in_yolofs(&["sh", "-c", "printf one > hello.txt; printf two > hello.txt"])
        .unwrap_or(1);

    watch.kill().ok();
    let output = watch.wait_with_output().expect("collecting watch output");
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    let expected_path = format!("{}/hello.txt", s.root.display());

    assert_eq!(code, 0, "writes should succeed when watch allows them");
    assert!(
        stderr.matches(&expected_path).count() >= 2,
        "write-ask should prompt for both writes to {expected_path}, got:\n{stderr}"
    );
    assert!(
        stderr.contains("rule: / asks before writes"),
        "write-ask prompt should show inherited rule source: {stderr}"
    );
}

/// Plain `ask` decisions are one-shot: allowing a read must not cache an
/// allow rule over the inherited ask policy.
#[test]
fn watch_allow_all_answers_each_plain_ask() {
    let s = YoloSession::new_with_config(Config {
        rules: BTreeMap::new(),
        ..Default::default()
    })
    .expect("session setup");

    let mut watch = std::process::Command::new(YOLO_BIN)
        .args(["watch", "--allow-all"])
        .current_dir(&s.root)
        .env("NO_COLOR", "1")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawning yolofs watch --allow-all");

    std::thread::sleep(Duration::from_millis(200));

    let first = std::fs::read_to_string(s.mnt_path("hello.txt"));
    let second = std::fs::read_to_string(s.mnt_path("hello.txt"));

    watch.kill().ok();
    let output = watch.wait_with_output().expect("collecting watch output");
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    let expected_path = format!("{}/hello.txt", s.root.display());

    first.expect("first read should succeed when watch allows it");
    second.expect("second read should succeed when watch allows it");
    assert!(
        stderr.matches(&expected_path).count() >= 2,
        "plain ask should prompt for both reads to {expected_path}, got:\n{stderr}"
    );
    assert!(
        stderr.contains("rule: default asks"),
        "default ask prompt should show default source: {stderr}"
    );
}

/// Starting a second watch while one is already running should fail
/// with a clear "already running" message (kernel returns EBUSY).
#[test]
fn second_watch_reports_already_running() {
    let s = YoloSession::new_with_config(Config {
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

    // Give it time to register with the kernel (ensure_ctl claims daemon_file).
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
        rules: BTreeMap::from([("/".into(), Perm::Allow)]),
        ..Default::default()
    })
    .expect("session setup");

    let file_path = "hello.txt".to_string();
    s.cli(&["rule", "ask", &file_path]).unwrap();
    (s, file_path)
}

fn assert_errno(result: anyhow::Result<()>, expected: nix::errno::Errno) {
    let err = result.expect_err("operation should fail");
    assert_eq!(
        err.downcast_ref::<nix::errno::Errno>(),
        Some(&expected),
        "expected {expected}, got {err:#}"
    );
}

/// Open a non-blocking ctl fd and claim daemon status with a first GET_ASK
/// (which must report EAGAIN). Claiming *before* spawning a reader thread
/// closes a race: with no daemon connected the kernel denies asks instantly
/// without enqueuing them, so a blocking GET_ASK issued after the reader
/// could wait forever for an ask that was already settled.
fn claim_daemon(s: &YoloSession) -> std::fs::File {
    let ctl_file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(s.root.join(".yolofs/mnt"))
        .expect("open nonblocking ioctl fd");
    assert_eq!(
        ioctl::get_ask(&ctl_file).expect_err("no ask should be pending yet"),
        nix::errno::Errno::EAGAIN
    );
    ctl_file
}

/// Dequeue the next ask from a non-blocking ctl fd, polling with a deadline
/// so a missing ask fails the test instead of hanging it.
fn poll_get_ask(ctl_file: &std::fs::File) -> ioctl::Ask {
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        match ioctl::get_ask(ctl_file) {
            Ok(req) => return req,
            Err(nix::errno::Errno::EAGAIN) if std::time::Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(e) => panic!("no ask delivered within the deadline: {e}"),
        }
    }
}

/// Interactive `yolofs watch` — daemon reads "y\n" from piped stdin and
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

    // Pre-fill stdin with "y" (allow).
    watch.stdin.as_mut().unwrap().write_all(b"y\n").unwrap();

    let content = std::fs::read_to_string(s.mnt_path("hello.txt"));

    watch.kill().ok();
    let output = watch.wait_with_output().unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();

    assert!(
        stderr.contains("wants to read"),
        "daemon should have prompted: {stderr}"
    );
    assert!(
        stderr.contains("rule: ") && stderr.contains("hello.txt asks"),
        "prompt should show explicit ask rule source: {stderr}"
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
        stderr.contains("wants to read"),
        "daemon should have prompted: {stderr}"
    );
    assert!(
        stderr.contains("→ deny"),
        "daemon should log deny decision: {stderr}"
    );
    assert!(result.is_err(), "read should fail after interactive 'deny'");
}

/// Unknown prompt input is explicit parser failure; the caller falls back to
/// deny and reports the unknown token.
#[test]
fn interactive_watch_unknown_input_denies_read() {
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
    watch.stdin.as_mut().unwrap().write_all(b"x\n").unwrap();

    let result = std::fs::read_to_string(s.mnt_path("hello.txt"));

    watch.kill().ok();
    let output = watch.wait_with_output().unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();

    assert!(
        stderr.contains("unknown: x, denying"),
        "daemon should report unknown input: {stderr}"
    );
    assert!(
        stderr.contains("→ deny"),
        "daemon should log deny decision: {stderr}"
    );
    assert!(result.is_err(), "read should fail after unknown input");
}

/// Ask decisions are a separate allow/deny enum; out-of-range raw values are
/// rejected.
#[test]
fn put_decision_rejects_invalid_decisions() {
    let (s, _path) = session_with_ask_file();
    let ctl_file = claim_daemon(&s);
    let path = s.mnt_path("hello.txt");

    let reader = std::thread::spawn(move || std::fs::read_to_string(path));
    let req = poll_get_ask(&ctl_file);

    assert_errno(
        ioctl::put_decision_raw(&ctl_file, req.id, 2),
        nix::errno::Errno::EINVAL,
    );
    assert_errno(
        ioctl::put_decision_raw(&ctl_file, req.id, u8::MAX),
        nix::errno::Errno::EINVAL,
    );
    ioctl::put_decision(&ctl_file, req.id, Decision::Deny).expect("deny should unblock the reader");

    let result = reader.join().expect("reader thread should not panic");
    assert!(result.is_err(), "read should fail after final deny");
}

/// Only the fd that claimed daemon ownership with GET_ASK may answer the ask.
#[test]
fn put_decision_rejects_non_daemon_fd() {
    let (s, _path) = session_with_ask_file();
    let daemon_fd = claim_daemon(&s);
    let other_fd = ioctl::open(&s.root.join(".yolofs")).expect("open second ioctl fd");
    let path = s.mnt_path("hello.txt");

    let reader = std::thread::spawn(move || std::fs::read_to_string(path));
    let req = poll_get_ask(&daemon_fd);

    assert_errno(
        ioctl::put_decision(&other_fd, req.id, Decision::Allow),
        nix::errno::Errno::EPERM,
    );
    ioctl::put_decision(&daemon_fd, req.id, Decision::Deny)
        .expect("daemon fd should answer the ask");

    let result = reader.join().expect("reader thread should not panic");
    assert!(result.is_err(), "read should fail after daemon deny");
}

/// Closing the daemon fd after dequeuing an ask denies the dispatched request.
#[test]
fn daemon_close_denies_dispatched_ask() {
    let (s, _path) = session_with_ask_file();
    let ctl_file = claim_daemon(&s);
    let path = s.mnt_path("hello.txt");

    let reader = std::thread::spawn(move || std::fs::read_to_string(path));
    let _req = poll_get_ask(&ctl_file);
    drop(ctl_file);

    let result = reader.join().expect("reader thread should not panic");
    assert!(result.is_err(), "read should fail after daemon fd close");
}

/// Closing a daemon fd also denies asks that were still pending and had not yet
/// been dequeued.
#[test]
fn daemon_close_denies_pending_ask() {
    let (s, _path) = session_with_ask_file();
    let ctl_file = claim_daemon(&s);

    let path = s.mnt_path("hello.txt");
    let reader = std::thread::spawn(move || std::fs::read_to_string(path));
    std::thread::sleep(Duration::from_millis(200));
    drop(ctl_file);

    let result = reader.join().expect("reader thread should not panic");
    assert!(result.is_err(), "read should fail after daemon fd close");
}

/// A connected daemon that never answers still gets the timeout default.
#[test]
fn dispatched_ask_times_out_to_deny() {
    let s = YoloSession::new_with_config(Config {
        prompt_timeout: Some(1),
        rules: BTreeMap::from([("/".into(), Perm::Allow)]),
        ..Default::default()
    })
    .expect("session setup");
    s.cli(&["rule", "ask", "hello.txt"]).unwrap();

    let ctl_file = claim_daemon(&s);
    let path = s.mnt_path("hello.txt");

    let reader = std::thread::spawn(move || std::fs::read_to_string(path));
    let req = poll_get_ask(&ctl_file);

    let result = reader.join().expect("reader thread should not panic");
    assert_errno(
        ioctl::put_decision(&ctl_file, req.id, Decision::Allow),
        nix::errno::Errno::ENOENT,
    );
    drop(ctl_file);
    assert!(result.is_err(), "read should fail after prompt timeout");
    assert_timeout_ask_note(&s);
}

/// A connected daemon that never dequeues a pending ask should not leave stale
/// work behind after timeout.
#[test]
fn pending_ask_times_out_and_is_removed() {
    let s = YoloSession::new_with_config(Config {
        prompt_timeout: Some(1),
        rules: BTreeMap::from([("/".into(), Perm::Allow)]),
        ..Default::default()
    })
    .expect("session setup");
    s.cli(&["rule", "ask", "hello.txt"]).unwrap();

    let ctl_file = claim_daemon(&s);

    let path = s.mnt_path("hello.txt");
    let reader = std::thread::spawn(move || std::fs::read_to_string(path));
    let result = reader.join().expect("reader thread should not panic");
    assert!(result.is_err(), "read should fail after prompt timeout");
    assert_eq!(
        ioctl::get_ask(&ctl_file).expect_err("timed-out ask should be removed"),
        nix::errno::Errno::EAGAIN
    );
    assert_timeout_ask_note(&s);
}

fn assert_timeout_ask_note(s: &YoloSession) {
    let journal = Journal::read(&s.root.join(".yolofs")).expect("read journal");
    let found = journal
        .segments
        .iter()
        .flat_map(|segment| segment.records.iter())
        .any(|record| {
            matches!(
                record,
                Record::Note(Note::Ask { path, op: Op::Read, decision: Decision::Deny })
                    if path.ends_with("/hello.txt")
            )
        });
    assert!(found, "expected timeout A note for hello.txt");
}

/// GET_ASK includes the rule source path and rule permission that caused the
/// prompt.
#[test]
fn get_ask_reports_rule_source() {
    let s = YoloSession::new_with_config(Config {
        rules: BTreeMap::from([("/".into(), Perm::WriteAsk)]),
        ..Default::default()
    })
    .expect("session setup");
    let ctl_file = claim_daemon(&s);
    let path = s.mnt_path("hello.txt");

    let writer = std::thread::spawn(move || std::fs::write(path, "modified\n"));
    let req = poll_get_ask(&ctl_file);

    assert_eq!(req.access_path, format!("{}/hello.txt", s.root.display()));
    assert_eq!(req.rule_path.as_deref(), Some("/"));
    assert_eq!(req.rule_perm, Perm::WriteAsk);

    ioctl::put_decision(&ctl_file, req.id, Decision::Deny).expect("deny should unblock the writer");
    let result = writer.join().expect("writer thread should not panic");
    assert!(result.is_err(), "write should fail after deny");
}

use crate::helpers::{Watch, YoloSession};
use std::collections::BTreeMap;
use std::os::unix::fs::OpenOptionsExt;
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
    let watch = Watch::spawn(&s.root, &["--allow-all"]);

    // touch creates a new file — the kernel must ask the daemon and receive
    // an allow response for this to succeed.
    let code = s.run_in_yolofs(&["touch", "a"]).unwrap_or(1);

    let stderr = watch.kill_and_collect();

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

    let watch = Watch::spawn(&s.root, &["--allow-all"]);

    let code = s
        .run_in_yolofs(&["sh", "-c", "printf one > hello.txt; printf two > hello.txt"])
        .unwrap_or(1);

    let stderr = watch.kill_and_collect();
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

    let watch = Watch::spawn(&s.root, &["--allow-all"]);

    let first = std::fs::read_to_string(s.mnt_path("hello.txt"));
    let second = std::fs::read_to_string(s.mnt_path("hello.txt"));

    let stderr = watch.kill_and_collect();
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

/// Open a non-blocking control fd on the mount root. Non-blocking so `ask_peek`
/// returns EAGAIN on an empty queue (via `poll_ask`) instead of hanging the
/// test; the initial peek also sanity-checks that the queue starts empty.
fn open_ctl(s: &YoloSession) -> std::fs::File {
    let ctl_file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(s.root.join(".yolofs/mnt"))
        .expect("open nonblocking ioctl fd");
    assert_eq!(
        ioctl::ask_peek(&ctl_file).expect_err("no ask should be pending yet"),
        nix::errno::Errno::EAGAIN
    );
    ctl_file
}

/// Peek the next ask from a non-blocking ctl fd, polling with a deadline so a
/// missing ask fails the test instead of hanging it. ASK_PEEK does not consume,
/// so the ask stays queued until `ask_decide` answers it.
fn poll_ask(ctl_file: &std::fs::File) -> ioctl::Ask {
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        match ioctl::ask_peek(ctl_file) {
            Ok(req) => return req,
            Err(nix::errno::Errno::EAGAIN) if std::time::Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(e) => panic!("no ask delivered within the deadline: {e}"),
        }
    }
}

/// `ask_peek` reads the head ask without removing it: repeated peeks return the
/// same ask, and it stays queued until `ask_decide` answers it.
#[test]
fn ask_peek_does_not_consume() {
    let (s, _path) = session_with_ask_file();
    let ctl_file = open_ctl(&s);

    let path = s.mnt_path("hello.txt");
    let reader = std::thread::spawn(move || std::fs::read_to_string(path));

    // Peek returns the ask; peeking again returns the *same* ask — not consumed.
    let first = poll_ask(&ctl_file);
    let second = poll_ask(&ctl_file);
    assert_eq!(first.id, second.id, "ask_peek must not consume the ask");

    ioctl::ask_decide(&ctl_file, first.id, Decision::Deny).expect("deny should unblock the reader");

    // Once answered, it is gone.
    assert_eq!(
        ioctl::ask_peek(&ctl_file).expect_err("answered ask should be removed"),
        nix::errno::Errno::EAGAIN
    );

    let result = reader.join().expect("reader thread should not panic");
    assert!(result.is_err(), "read should fail after deny");
}

/// Ask decisions are a separate allow/deny enum; out-of-range raw values are
/// rejected.
#[test]
fn ask_decide_rejects_invalid_decisions() {
    let (s, _path) = session_with_ask_file();
    let ctl_file = open_ctl(&s);
    let path = s.mnt_path("hello.txt");

    let reader = std::thread::spawn(move || std::fs::read_to_string(path));
    let req = poll_ask(&ctl_file);

    assert_errno(
        ioctl::ask_decide_raw(&ctl_file, req.id, 2),
        nix::errno::Errno::EINVAL,
    );
    assert_errno(
        ioctl::ask_decide_raw(&ctl_file, req.id, u8::MAX),
        nix::errno::Errno::EINVAL,
    );
    ioctl::ask_decide(&ctl_file, req.id, Decision::Deny).expect("deny should unblock the reader");

    let result = reader.join().expect("reader thread should not panic");
    assert!(result.is_err(), "read should fail after final deny");
}

/// A connected daemon that peeked an ask but never answers still gets the
/// timeout default, and a late answer for it fails with ENOENT.
#[test]
fn peeked_ask_times_out_to_deny() {
    let s = YoloSession::new_with_config(Config {
        prompt_timeout: Some(0.1),
        rules: BTreeMap::from([("/".into(), Perm::Allow)]),
        ..Default::default()
    })
    .expect("session setup");
    s.cli(&["rule", "ask", "hello.txt"]).unwrap();

    let ctl_file = open_ctl(&s);
    let path = s.mnt_path("hello.txt");

    let reader = std::thread::spawn(move || std::fs::read_to_string(path));
    let req = poll_ask(&ctl_file);

    let result = reader.join().expect("reader thread should not panic");
    assert_errno(
        ioctl::ask_decide(&ctl_file, req.id, Decision::Allow),
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
        prompt_timeout: Some(0.1),
        rules: BTreeMap::from([("/".into(), Perm::Allow)]),
        ..Default::default()
    })
    .expect("session setup");
    s.cli(&["rule", "ask", "hello.txt"]).unwrap();

    let ctl_file = open_ctl(&s);

    let path = s.mnt_path("hello.txt");
    let reader = std::thread::spawn(move || std::fs::read_to_string(path));
    let result = reader.join().expect("reader thread should not panic");
    assert!(result.is_err(), "read should fail after prompt timeout");
    assert_eq!(
        ioctl::ask_peek(&ctl_file).expect_err("timed-out ask should be removed"),
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

/// ASK_PEEK includes the rule source path and rule permission that caused the
/// prompt.
#[test]
fn ask_peek_reports_rule_source() {
    let s = YoloSession::new_with_config(Config {
        rules: BTreeMap::from([("/".into(), Perm::WriteAsk)]),
        ..Default::default()
    })
    .expect("session setup");
    let ctl_file = open_ctl(&s);
    let path = s.mnt_path("hello.txt");

    let writer = std::thread::spawn(move || std::fs::write(path, "modified\n"));
    let req = poll_ask(&ctl_file);

    assert_eq!(req.access_path, format!("{}/hello.txt", s.root.display()));
    assert_eq!(req.rule_path.as_deref(), Some("/"));
    assert_eq!(req.rule_perm, Perm::WriteAsk);

    ioctl::ask_decide(&ctl_file, req.id, Decision::Deny).expect("deny should unblock the writer");
    let result = writer.join().expect("writer thread should not panic");
    assert!(result.is_err(), "write should fail after deny");
}

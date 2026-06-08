//! Verify the kernel writes observational journal notes:
//!
//! - **B (Blocked)** records for accesses denied by yolofs rules, and
//! - **A (Ask resolved)** records carrying the decision an `ask` path
//!   resolved to (from the daemon or the no-daemon default).
//!
//! Both are observational — they do not set the dirty flag and they don't
//! contribute to the dir tree. The A-note tests at the bottom of this file
//! drive a live `yolo watch` daemon to confirm the daemon's interactive
//! decision is the one persisted in the journal.

use crate::helpers::YoloSession;
use crate::internals::helpers::{actions, journal, notes};
use std::collections::BTreeMap;
use std::fs;
use yolofs::config::Config;
use yolofs::journal::{Journal, Note, Op};
use yolofs::perm::Perm;

/// Build a session where the entire mount denies access.
fn deny_session() -> YoloSession {
    YoloSession::new_with_config(Config {
        rules: BTreeMap::from([("/".into(), Perm::Deny)]),
        ..Default::default()
    })
    .expect("session setup")
}

/// Build a session where the entire mount is read-only.
fn ro_session() -> YoloSession {
    YoloSession::new_with_config(Config {
        rules: BTreeMap::from([("/".into(), Perm::ReadOnly)]),
        ..Default::default()
    })
    .expect("session setup")
}

/// Block record format: B\0<path>\0<op>\n (3 fields).
#[test]
fn block_record_format() {
    let s = deny_session();

    let _ = fs::read_to_string(s.mnt_path("hello.txt"));

    let bytes = fs::read(s.root.join(".yolofs/journal")).expect("read journal");
    let block_lines: Vec<&[u8]> = bytes
        .split(|&b| b == b'\n')
        .filter(|line| !line.is_empty() && line[0] == b'B')
        .collect();
    assert!(!block_lines.is_empty(), "expected at least one B record");
    for line in &block_lines {
        let fields: Vec<&[u8]> = line.split(|&b| b == 0).collect();
        assert_eq!(
            fields.len(),
            3,
            "B record should be (B, path, op), got {} fields: {:?}",
            fields.len(),
            fields
                .iter()
                .map(|f| String::from_utf8_lossy(f))
                .collect::<Vec<_>>()
        );
        assert_eq!(fields[0], b"B");
        assert!(!fields[1].is_empty(), "path field must be non-empty");
        // op is a single letter: 'r' (read) or 'w' (write).
        assert!(
            fields[2] == b"r" || fields[2] == b"w",
            "op should be r/w, got {:?}",
            String::from_utf8_lossy(fields[2])
        );
    }
}

/// An `ask` path resolved with no daemon connected emits an A (ask) note
/// carrying the op and the applied default decision.
#[test]
fn ask_record_emitted_on_no_daemon() {
    // No rules → everything resolves to `ask`; no `yolo watch` daemon is
    // running, so the kernel denies (unanswered ask) and logs an A note.
    let s = YoloSession::new_with_config(Config {
        rules: BTreeMap::new(),
        ..Default::default()
    })
    .expect("session setup");

    let _ = fs::read_to_string(s.mnt_path("hello.txt"));

    let j = journal(&s);
    let ns = notes(&j);
    assert!(
        ns.iter().any(|n| matches!(
            n,
            Note::Ask { path, op, decision }
                if path.ends_with("/hello.txt")
                    && *op == Op::Read
                    && *decision == Perm::Deny
        )),
        "expected an A note (read -> deny) for hello.txt, got: {ns:?}"
    );
}

/// A denied read of a regular file produces a B record at the *target* path.
#[test]
fn denied_read_emits_block_for_file_path() {
    let s = deny_session();

    let _ = fs::read_to_string(s.mnt_path("hello.txt"));

    let j = journal(&s);
    let bs = notes(&j);
    assert!(
        bs.iter()
            .any(|n| matches!(n, Note::Block { path, .. } if path.ends_with("/hello.txt"))),
        "expected B for hello.txt, got: {:?}",
        bs
    );
}

/// A denied write under a READ rule produces a B record.
#[test]
fn ro_write_emits_block() {
    let s = ro_session();

    let _ = fs::write(s.mnt_path("hello.txt"), "x");

    let j = journal(&s);
    let bs = notes(&j);
    assert!(
        bs.iter()
            .any(|n| matches!(n, Note::Block { path, .. } if path.ends_with("/hello.txt"))),
        "expected B for hello.txt under ro+write, got: {:?}",
        bs
    );
}

/// A denied create logs the *child* path, not the parent.
#[test]
fn denied_create_emits_block_for_child_path() {
    let s = deny_session();

    let _ = fs::write(s.mnt_path("new_file.txt"), "x");

    let j = journal(&s);
    let bs = notes(&j);
    assert!(
        bs.iter()
            .any(|n| matches!(n, Note::Block { path, .. } if path.ends_with("/new_file.txt"))),
        "expected B for new_file.txt (child), got: {:?}",
        bs
    );
    // The mutate-denial path must record the *child* target, never just
    // the parent directory. Confirm no record points at the parent only.
    assert!(
        !bs.iter().any(|n| matches!(n, Note::Block { path, .. }
            if !path.ends_with("/new_file.txt") && !path.ends_with(".txt"))),
        "should not log parent path for mutate denial: {:?}",
        bs
    );
}

/// A denied unlink logs the *target* path.
#[test]
fn denied_unlink_emits_block_for_target_path() {
    let s = ro_session();

    let _ = fs::remove_file(s.mnt_path("hello.txt"));

    let j = journal(&s);
    let bs = notes(&j);
    assert!(
        bs.iter()
            .any(|n| matches!(n, Note::Block { path, .. } if path.ends_with("/hello.txt"))),
        "expected B for hello.txt, got: {:?}",
        bs
    );
}

/// B records do not affect the dir tree (no Action contribution).
#[test]
fn block_records_do_not_contribute_to_tree() {
    let s = deny_session();

    for _ in 0..5 {
        let _ = fs::read_to_string(s.mnt_path("hello.txt"));
    }

    let j = journal(&s);
    let bs = notes(&j);
    assert!(!bs.is_empty(), "expected B records");
    let acts = actions(&j);
    assert!(
        acts.is_empty(),
        "denied reads must not produce S/D/R actions, got: {:?}",
        acts
    );
}

/// B records do not set sbi->dirty: an auto-snapshot (SNAPSHOT_IF_CHANGED)
/// must be skipped if only B writes happened since the last marker.
///
/// We check this indirectly via `yolo exec`: a denied-read command produces
/// only B records, and the kernel's SNAPSHOT_IF_CHANGED auto-snapshot should
/// be skipped, leaving the journal with no M record.
#[test]
fn block_writes_do_not_set_dirty() {
    use crate::helpers::YOLO_BIN;
    let s = YoloSession::new_with_config(Config {
        rules: BTreeMap::from([("/".into(), Perm::Deny)]),
        auto_snapshot: true,
        ..Default::default()
    })
    .expect("session setup");

    // Run a command that triggers only denied reads.
    let status = std::process::Command::new(YOLO_BIN)
        .args(["exec", "--", "sh", "-c", "cat hello.txt; true"])
        .current_dir(&s.root)
        .env("NO_COLOR", "1")
        .status()
        .expect("yolo exec");
    let _ = status;

    let j = journal(&s);
    let bs = notes(&j);
    assert!(
        !bs.is_empty(),
        "expected B records from the denied cat invocation"
    );

    // Phantom marker is index 0; if SNAPSHOT_IF_CHANGED skipped correctly, no
    // real M record should exist.  markers.len() == 1 means only the phantom.
    assert_eq!(
        j.markers.len(),
        1,
        "SNAPSHOT_IF_CHANGED should skip auto-snapshot when only B records were written; markers={:?}",
        j.markers.iter().collect::<Vec<_>>()
    );
}

/// Inverse of the above: when real mutations occur, dirty IS set and the
/// auto-snapshot after `yolo exec` runs. B records in the same session
/// must not cancel that out.
#[test]
fn mixed_mutations_and_blocks_still_set_dirty() {
    use crate::helpers::YOLO_BIN;
    // Manual setup so we can install a deny rule on a specific file
    // whose host path canonicalizes correctly.
    let root = tempfile::tempdir().unwrap().keep();
    fs::write(root.join("locked.txt"), "secret\n").unwrap();

    let mut rules = BTreeMap::new();
    // Allow everything by default so the new.txt write succeeds, except the
    // explicit deny on locked.txt (more specific, so it wins).
    rules.insert("/".into(), Perm::Allow);
    rules.insert(
        root.join("locked.txt").to_string_lossy().into_owned(),
        Perm::Deny,
    );
    let config = Config {
        permission: true,
        auto_snapshot: true,
        rules,
        ..Default::default()
    };
    config.save(&root.join("yolofs.toml")).unwrap();
    let output = std::process::Command::new(YOLO_BIN)
        .arg("mount")
        .current_dir(&root)
        .env("NO_COLOR", "1")
        .output()
        .unwrap();
    assert!(output.status.success(), "mount failed: {:?}", output);
    let s = YoloSession::from_existing_root(root).expect("session from root");

    let status = std::process::Command::new(YOLO_BIN)
        .args([
            "exec",
            "--",
            "sh",
            "-c",
            "cat locked.txt; echo data > new.txt; true",
        ])
        .current_dir(&s.root)
        .env("NO_COLOR", "1")
        .status()
        .expect("yolo exec");
    let _ = status;

    let j = journal(&s);
    let bs = notes(&j);
    let acts = actions(&j);
    assert!(!bs.is_empty(), "expected B from denied locked.txt read");
    assert!(
        !acts.is_empty(),
        "expected at least one S record from /new.txt write"
    );
    // Auto-snapshot should fire because dirty was set by the S record.
    assert!(
        j.markers.len() >= 2,
        "expected auto-snapshot when mutations occur alongside notes; markers={:?}",
        j.markers.iter().collect::<Vec<_>>()
    );
}

/// HIDE paths return -ENOENT, not -EACCES, and must NOT produce B records.
/// This protects the "no logging for HIDE" non-goal from accidental
/// regressions.
#[test]
fn hidden_paths_do_not_log_block() {
    use crate::helpers::YOLO_BIN;
    // Manual setup — rule paths must canonicalize on the host, so use
    // an absolute host path inside the temp root.
    let root = tempfile::tempdir().unwrap().keep();
    fs::write(root.join("hello.txt"), "base\n").unwrap();
    fs::write(root.join("visible.txt"), "ok\n").unwrap();

    let mut rules = BTreeMap::new();
    rules.insert(
        root.join("hello.txt").to_string_lossy().into_owned(),
        Perm::Hide,
    );
    // Permissive backdrop so visible.txt works; the hide rule above wins.
    rules.insert("/".into(), Perm::Allow);
    let config = Config {
        permission: true,
        rules,
        ..Default::default()
    };
    config.save(&root.join("yolofs.toml")).unwrap();
    let output = std::process::Command::new(YOLO_BIN)
        .arg("mount")
        .current_dir(&root)
        .env("NO_COLOR", "1")
        .output()
        .unwrap();
    assert!(output.status.success(), "mount failed: {:?}", output);
    let s = YoloSession::from_existing_root(root).expect("session from root");

    // Any access to the hidden path returns ENOENT, not EACCES.
    let result = fs::read_to_string(s.mnt_path("hello.txt"));
    assert!(
        result.is_err(),
        "hidden file should not be readable: {:?}",
        result
    );
    let _ = fs::metadata(s.mnt_path("hello.txt"));

    let j = journal(&s);
    let bs = notes(&j);
    assert!(
        bs.is_empty(),
        "HIDE -> -ENOENT must not log any B record, got: {:?}",
        bs
    );
}

/// `ask` resolving to deny via the default policy exercises the
/// `yolo_open` emit site (which catches the case `yolo_permission` doesn't,
/// because `yolo_permission` returns 0 for ASK and lets `yolo_open` resolve).
#[test]
fn ask_resolved_to_default_deny_emits_block() {
    // No daemon connected; the ask is denied immediately when an
    // unruled file is opened. The deny decision is made inside
    // `yolo_check_dentry_perm` and surfaces from `yolo_open`, not from
    // `yolo_permission` (which returned 0 for the ASK perm).
    let s = YoloSession::new_with_config(Config {
        rules: BTreeMap::new(),
        ..Default::default()
    })
    .expect("session setup");

    let _ = fs::read_to_string(s.mnt_path("hello.txt"));

    let j = journal(&s);
    let bs = notes(&j);
    assert!(
        bs.iter()
            .any(|n| matches!(n, Note::Block { path, .. } if path.ends_with("/hello.txt"))),
        "expected B for ask-resolved-deny on hello.txt, got: {:?}",
        bs
    );
}

/// B records surface in `yolo review` (as observed-but-not-staged accesses)
/// but never in `yolo review --diff`, which shows staged content only. Repeated
/// identical blocks are deduped to a single summary line.
#[test]
fn block_records_shown_in_status_hidden_in_diff() {
    use crate::helpers::YOLO_BIN;
    let s = deny_session();

    for _ in 0..3 {
        let _ = fs::read_to_string(s.mnt_path("hello.txt"));
    }
    // Sanity: B records did get emitted.
    let j = journal(&s);
    assert!(!notes(&j).is_empty(), "expected B records to be emitted");

    let status = std::process::Command::new(YOLO_BIN)
        .args(["review"])
        .current_dir(&s.root)
        .env("NO_COLOR", "1")
        .output()
        .expect("yolo review");
    let stdout = String::from_utf8_lossy(&status.stdout);
    assert!(
        stdout.contains("blocked") && stdout.contains("hello.txt"),
        "yolo review should surface the B record: {stdout}"
    );
    // Three identical blocked reads collapse to one status line.
    assert_eq!(
        stdout.matches("blocked").count(),
        1,
        "identical blocks should be deduped in status: {stdout}"
    );

    let diff = std::process::Command::new(YOLO_BIN)
        .args(["review", "--diff"])
        .current_dir(&s.root)
        .env("NO_COLOR", "1")
        .output()
        .expect("yolo review --diff");
    let stdout = String::from_utf8_lossy(&diff.stdout);
    assert!(
        !stdout.contains("blocked") && !stdout.contains("hello.txt"),
        "yolo review --diff must not show B records (staged content only): {stdout}"
    );
}

/// A (ask-resolved) notes also surface in `yolo review`. With no daemon the
/// ask resolves to deny, producing an A note that status should display.
#[test]
fn ask_records_shown_in_status() {
    let s = YoloSession::new_with_config(Config {
        rules: BTreeMap::new(),
        ..Default::default()
    })
    .expect("session setup");

    let _ = fs::read_to_string(s.mnt_path("hello.txt"));

    // Sanity: an A note was emitted.
    assert!(
        !notes(&journal(&s)).is_empty(),
        "expected an A note to be emitted"
    );

    let out = s.cli(&["review"]).expect("yolo review");
    assert!(
        out.contains("ask") && out.contains("hello.txt"),
        "yolo review should surface the A note: {out}"
    );
}

// ── A (ask) notes: daemon decision → journal round-trip ─────────────
//
// `ask_record_emitted_on_no_daemon` above covers the *no-daemon default*
// path (decision = deny). These tests confirm the inverse: when a live
// `yolo watch` daemon answers an ask interactively, the decision it gives
// (allow / read-only / deny) is exactly what the kernel records in the
// A note. This ties the userspace daemon (cli/test_watch.rs) to the journal
// recording verified here.

/// Mount with "/" = Allow (so directory traversal never asks), then live-add
/// an `ask` rule for a single file so only that file's open() goes through
/// the daemon. Mirrors `session_with_ask_file` in cli/test_watch.rs.
fn session_with_ask_file() -> YoloSession {
    let s = YoloSession::new_with_config(Config {
        rules: BTreeMap::from([("/".into(), Perm::Allow)]),
        ..Default::default()
    })
    .expect("session setup");
    s.cli(&["rule", "ask", "hello.txt"]).unwrap();
    s
}

/// Spawn `yolo watch`, pre-fill its stdin with `input` (one decision per
/// line), give it time to block on the ioctl read, and return the child so
/// the caller can trigger the ask and then stop the daemon.
fn spawn_watch_with_input(s: &YoloSession, input: &str) -> std::process::Child {
    use crate::helpers::YOLO_BIN;
    use std::io::Write;
    use std::process::{Command, Stdio};

    let mut watch = Command::new(YOLO_BIN)
        .args(["watch"])
        .current_dir(&s.root)
        .env("NO_COLOR", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn watch");

    // Let the daemon open the ioctl fd and block on the first read.
    std::thread::sleep(std::time::Duration::from_millis(200));
    watch
        .stdin
        .as_mut()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    watch
}

/// Assert the journal contains an A note for `suffix` with the given op and
/// resolved decision.
fn assert_ask_note(j: &Journal, suffix: &str, op: Op, decision: Perm) {
    let ns = notes(j);
    assert!(
        ns.iter().any(|n| matches!(
            n,
            Note::Ask { path, op: o, decision: d }
                if path.ends_with(suffix) && *o == op && *d == decision
        )),
        "expected A note {suffix} ({op:?}) -> {decision:?}, got: {ns:?}"
    );
}

/// Daemon answers `allow` → read succeeds and the A note records `allow`.
#[test]
fn daemon_allow_records_ask_note_allow() {
    let s = session_with_ask_file();
    let mut watch = spawn_watch_with_input(&s, "a\n");

    let content = fs::read_to_string(s.mnt_path("hello.txt"));

    watch.kill().ok();
    let _ = watch.wait();

    assert_eq!(
        content.expect("read should succeed after daemon allow"),
        "base content\n"
    );
    assert_ask_note(&journal(&s), "/hello.txt", Op::Read, Perm::Allow);
}

/// Daemon answers `read` → read succeeds and the A note records `read`.
#[test]
fn daemon_read_records_ask_note_read() {
    let s = session_with_ask_file();
    let mut watch = spawn_watch_with_input(&s, "r\n");

    let content = fs::read_to_string(s.mnt_path("hello.txt"));

    watch.kill().ok();
    let _ = watch.wait();

    assert_eq!(
        content.expect("read should succeed after daemon read-only"),
        "base content\n"
    );
    assert_ask_note(&journal(&s), "/hello.txt", Op::Read, Perm::ReadOnly);
}

/// Daemon answers `write-ask` → read succeeds and the A note records
/// `write-ask`.
#[test]
fn daemon_write_ask_records_ask_note_write_ask() {
    let s = session_with_ask_file();
    let mut watch = spawn_watch_with_input(&s, "w\n");

    let content = fs::read_to_string(s.mnt_path("hello.txt"));

    watch.kill().ok();
    let _ = watch.wait();

    assert_eq!(
        content.expect("read should succeed after daemon write-ask"),
        "base content\n"
    );
    assert_ask_note(&journal(&s), "/hello.txt", Op::Read, Perm::WriteAsk);
}

/// Daemon answers `deny` → read fails (EACCES) and the A note records `deny`.
#[test]
fn daemon_deny_records_ask_note_deny() {
    let s = session_with_ask_file();
    let mut watch = spawn_watch_with_input(&s, "d\n");

    let result = fs::read_to_string(s.mnt_path("hello.txt"));

    watch.kill().ok();
    let _ = watch.wait();

    assert!(result.is_err(), "read should fail after daemon deny");
    assert_ask_note(&journal(&s), "/hello.txt", Op::Read, Perm::Deny);
}

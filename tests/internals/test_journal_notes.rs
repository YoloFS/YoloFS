//! Verify the kernel writes observational journal notes:
//!
//! - **G (Gate result)** records for prompted and statically denied accesses,
//! - **C (Configure)** records for live explicit-policy assignments.
//!
//! Both are observational — they do not set the dirty flag and they don't
//! contribute to the dir tree. The G-note tests at the bottom of this file
//! drive a live `yolo watch` daemon to confirm the daemon's interactive
//! decision is the one persisted in the journal.

use crate::helpers::YoloSession;
use crate::internals::helpers::{actions, journal, notes};
use std::collections::BTreeMap;
use std::fs;
use yolofs::config::Config;
use yolofs::ioctl;
use yolofs::journal::{GateResult, Journal, Note, Op, Policy};
use yolofs::perm::{Decision, Perm};

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

/// Gate record format: G\0<path>\0<op>\0<result>\n (4 fields).
#[test]
fn gate_record_format() {
    let s = deny_session();

    let _ = fs::read_to_string(s.mnt_path("hello.txt"));

    let bytes = fs::read(s.root.join(".yolofs/journal")).expect("read journal");
    let gate_lines: Vec<&[u8]> = bytes
        .split(|&b| b == b'\n')
        .filter(|line| !line.is_empty() && line[0] == b'G')
        .collect();
    assert!(!gate_lines.is_empty(), "expected at least one G record");
    for line in &gate_lines {
        let fields: Vec<&[u8]> = line.split(|&b| b == 0).collect();
        assert_eq!(
            fields.len(),
            4,
            "G record should be (G, path, op, result), got {} fields: {:?}",
            fields.len(),
            fields
                .iter()
                .map(|f| String::from_utf8_lossy(f))
                .collect::<Vec<_>>()
        );
        assert_eq!(fields[0], b"G");
        assert!(!fields[1].is_empty(), "path field must be non-empty");
        // op is a single letter: 'r' (read) or 'w' (write).
        assert!(
            fields[2] == b"r" || fields[2] == b"w",
            "op should be r/w, got {:?}",
            String::from_utf8_lossy(fields[2])
        );
        assert_eq!(fields[3], b"d", "static denial result should be d");
    }
}

/// An `ask` path resolved with no daemon connected emits a G note
/// carrying the op and the applied default decision.
#[test]
fn ask_record_emitted_on_no_daemon() {
    // No rules → everything resolves to `ask`; no `yolo watch` daemon is
    // running, so the kernel denies (unanswered ask) and logs G with n.
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
            Note::Gate { path, op, result }
                if path.ends_with("/hello.txt")
                    && *op == Op::Read
                    && *result == GateResult::AskDeny
        )),
        "expected G(read, n) for hello.txt, got: {ns:?}"
    );
}

/// A denied read produces G with d at the target path.
#[test]
fn denied_read_emits_gate_for_file_path() {
    let s = deny_session();

    let _ = fs::read_to_string(s.mnt_path("hello.txt"));

    let j = journal(&s);
    let bs = notes(&j);
    assert!(
        bs.iter()
            .any(|n| matches!(n, Note::Gate { path, result: GateResult::DirectDeny, .. } if path.ends_with("/hello.txt"))),
        "expected G(..., d) for hello.txt, got: {:?}",
        bs
    );
}

/// A denied write under a read-only rule produces G with d.
#[test]
fn ro_write_emits_gate() {
    let s = ro_session();

    let _ = fs::write(s.mnt_path("hello.txt"), "x");

    let j = journal(&s);
    let bs = notes(&j);
    assert!(
        bs.iter()
            .any(|n| matches!(n, Note::Gate { path, result: GateResult::DirectDeny, .. } if path.ends_with("/hello.txt"))),
        "expected G(..., d) for hello.txt under ro+write, got: {:?}",
        bs
    );
}

/// A denied create logs the child path, not the parent.
#[test]
fn denied_create_emits_gate_for_child_path() {
    let s = deny_session();

    let _ = fs::write(s.mnt_path("new_file.txt"), "x");

    let j = journal(&s);
    let bs = notes(&j);
    assert!(
        bs.iter()
            .any(|n| matches!(n, Note::Gate { path, .. } if path.ends_with("/new_file.txt"))),
        "expected G for new_file.txt (child), got: {:?}",
        bs
    );
    // The mutate-denial path must record the *child* target, never just
    // the parent directory. Confirm no record points at the parent only.
    assert!(
        !bs.iter().any(|n| matches!(n, Note::Gate { path, .. }
            if !path.ends_with("/new_file.txt") && !path.ends_with(".txt"))),
        "should not log parent path for mutate denial: {:?}",
        bs
    );
}

/// A denied unlink logs the *target* path.
#[test]
fn denied_unlink_emits_gate_for_target_path() {
    let s = ro_session();

    let _ = fs::remove_file(s.mnt_path("hello.txt"));

    let j = journal(&s);
    let bs = notes(&j);
    assert!(
        bs.iter()
            .any(|n| matches!(n, Note::Gate { path, .. } if path.ends_with("/hello.txt"))),
        "expected G for hello.txt, got: {:?}",
        bs
    );
}

/// G records do not affect the dir tree (no Action contribution).
#[test]
fn gate_records_do_not_contribute_to_tree() {
    let s = deny_session();

    for _ in 0..5 {
        let _ = fs::read_to_string(s.mnt_path("hello.txt"));
    }

    let j = journal(&s);
    let bs = notes(&j);
    assert!(!bs.is_empty(), "expected G records");
    let acts = actions(&j);
    assert!(
        acts.is_empty(),
        "denied reads must not produce S/D/R actions, got: {:?}",
        acts
    );
}

/// G records do not set sbi->dirty: an auto-snapshot (SNAPSHOT_IF_CHANGED)
/// must be skipped if only G writes happened since the last marker.
///
/// We check this indirectly via `yolo run -- <cmd>`: a denied-read command produces
/// only G records, and the kernel's SNAPSHOT_IF_CHANGED auto-snapshot should
/// be skipped, leaving the journal with no M record.
#[test]
fn gate_writes_do_not_set_dirty() {
    use crate::helpers::YOLO_BIN;
    let s = YoloSession::new_with_config(Config {
        rules: BTreeMap::from([("/".into(), Perm::Deny)]),
        auto_snapshot: true,
        ..Default::default()
    })
    .expect("session setup");

    // Run a command that triggers only denied reads.
    let status = std::process::Command::new(YOLO_BIN)
        .args([
            "run",
            "--no-review",
            "--",
            "sh",
            "-c",
            "cat hello.txt; true",
        ])
        .current_dir(&s.root)
        .env("NO_COLOR", "1")
        .status()
        .expect("yolo run --no-review -- cmd");
    let _ = status;

    let j = journal(&s);
    let bs = notes(&j);
    assert!(
        !bs.is_empty(),
        "expected G records from the denied cat invocation"
    );

    // Phantom marker is index 0; if SNAPSHOT_IF_CHANGED skipped correctly, no
    // real M record should exist.  markers.len() == 1 means only the phantom.
    assert_eq!(
        j.markers.len(),
        1,
        "SNAPSHOT_IF_CHANGED should skip auto-snapshot when only G records were written; markers={:?}",
        j.markers.iter().collect::<Vec<_>>()
    );
}

/// Inverse of the above: when real mutations occur, dirty IS set and the
/// auto-snapshot after the run completes. G records in the same session
/// must not cancel that out.
#[test]
fn mixed_mutations_and_gates_still_set_dirty() {
    use crate::helpers::YOLO_BIN;
    // Manual setup so we can install a deny rule on a specific file
    // whose host path canonicalizes correctly.
    let root = crate::helpers::session_tempdir().unwrap().keep();
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
            "run",
            "--no-review",
            "--",
            "sh",
            "-c",
            "cat locked.txt; echo data > new.txt; true",
        ])
        .current_dir(&s.root)
        .env("NO_COLOR", "1")
        .status()
        .expect("yolo run --no-review -- cmd");
    let _ = status;

    let j = journal(&s);
    let bs = notes(&j);
    let acts = actions(&j);
    assert!(!bs.is_empty(), "expected G from denied locked.txt read");
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

/// An ask denial emits exactly one G with result n, not a second direct-deny G.
#[test]
fn ask_deny_records_one_gate_result() {
    // No rules → everything resolves to `ask`; no daemon → the ask is denied
    // immediately when an unruled file is opened.
    let s = YoloSession::new_with_config(Config {
        rules: BTreeMap::new(),
        ..Default::default()
    })
    .expect("session setup");

    let _ = fs::read_to_string(s.mnt_path("hello.txt"));

    let j = journal(&s);
    let ns = notes(&j);
    // Exactly one G (read, asked no) for hello.txt.
    let asks: Vec<_> = ns
        .iter()
        .filter(|n| {
            matches!(n, Note::Gate { path, op, result: GateResult::AskDeny }
            if path.ends_with("/hello.txt") && *op == Op::Read)
        })
        .collect();
    assert_eq!(
        asks.len(),
        1,
        "expected exactly one G(read, n), got: {ns:?}"
    );
    assert!(
        !ns.iter()
            .any(|n| matches!(n, Note::Gate { path, result: GateResult::DirectDeny, .. } if path.ends_with("/hello.txt"))),
        "ask denial must not emit a direct-deny G, got: {ns:?}"
    );
}

/// A live `yolo rule` assignment emits C, then a denied access emits G.
#[test]
fn live_rule_assignment_emits_configure_then_gate() {
    let s = YoloSession::new_with_config(Config {
        rules: BTreeMap::from([("/".into(), Perm::Allow)]),
        ..Default::default()
    })
    .expect("session setup");
    s.cli(&["rule", "deny", "subdir"])
        .expect("install deny rule");

    let _ = fs::read_to_string(s.mnt_path("subdir/deep.txt"));

    let j = journal(&s);
    let ns = notes(&j);
    assert!(
        ns.iter().any(|n| matches!(
            n,
            Note::Configure { path, policy: Policy::Deny }
                if path.ends_with("/subdir")
        )),
        "expected C for the deny assignment, got: {ns:?}"
    );
    assert!(
        ns.iter().any(|n| matches!(
            n,
            Note::Gate { path, result: GateResult::DirectDeny, .. }
                if path.ends_with("/subdir/deep.txt")
        )),
        "expected direct-deny G for subdir/deep.txt, got: {ns:?}"
    );
}

#[test]
fn configure_record_format_and_noop_suppression() {
    let s = YoloSession::new_with_config(Config {
        rules: BTreeMap::from([("/".into(), Perm::Allow)]),
        ..Default::default()
    })
    .expect("session setup");

    // Mount-time configuration is initialization, not a C event.
    assert!(notes(&journal(&s)).is_empty());

    // Each step: the command and the C policy it should append, or None when
    // no C record is expected (an exact no-op).
    let steps: &[(&str, Option<Policy>)] = &[
        ("ask", Some(Policy::Ask)),
        ("allow", Some(Policy::Allow)),
        ("write-ask", Some(Policy::WriteAsk)),
        ("read-only", Some(Policy::ReadOnly)),
        ("deny", Some(Policy::Deny)),
        ("deny", None), // exact no-op: no C
        ("unset", Some(Policy::Unset)),
    ];
    let mut expected_c: Vec<Policy> = Vec::new();
    for (command, expected) in steps {
        s.cli(&["rule", command, "subdir"])
            .unwrap_or_else(|e| panic!("install {command} rule: {e}"));
        if let Some(policy) = expected {
            expected_c.push(*policy);
        }
        let configured: Vec<Policy> = notes(&journal(&s))
            .into_iter()
            .filter_map(|note| match note {
                Note::Configure { policy, .. } => Some(*policy),
                Note::Gate { .. } => None,
            })
            .collect();
        assert_eq!(configured, expected_c, "C sequence after `rule {command}`");
    }
    s.cli(&["snapshot", "--if-changed", "notes-only"])
        .expect("C-only activity should not snapshot");

    let j = journal(&s);
    assert_eq!(j.markers.len(), 1, "C records must not set the dirty bit");
    let configured: Vec<_> = notes(&j)
        .into_iter()
        .filter_map(|note| match note {
            Note::Configure { path, policy } => Some((path.as_str(), *policy)),
            Note::Gate { .. } => None,
        })
        .collect();
    assert_eq!(configured.len(), 6, "an exact no-op emits no C");
    assert!(configured.iter().all(|(path, _)| path.ends_with("/subdir")));
    assert_eq!(
        configured
            .iter()
            .map(|(_, policy)| *policy)
            .collect::<Vec<_>>(),
        vec![
            Policy::Ask,
            Policy::Allow,
            Policy::WriteAsk,
            Policy::ReadOnly,
            Policy::Deny,
            Policy::Unset,
        ]
    );

    let bytes = fs::read(s.root.join(".yolofs/journal")).expect("read journal");
    let policy_codes: Vec<_> = bytes
        .split(|&b| b == b'\n')
        .filter(|line| !line.is_empty() && line[0] == b'C')
        .map(|line| {
            let fields: Vec<_> = line.split(|&b| b == 0).collect();
            assert_eq!(fields.len(), 3, "C is (tag, path, policy)");
            assert_eq!(fields[0], b"C");
            assert!(!fields[1].is_empty());
            assert_eq!(fields[2].len(), 1, "C policy must be one byte");
            fields[2][0]
        })
        .collect();
    assert_eq!(policy_codes, b"qawrdu");
}

/// A journaled live assignment is published only after C was fully appended.
#[test]
fn configure_append_failure_preserves_old_live_policy() {
    let s = YoloSession::new_with_config(Config {
        rules: BTreeMap::from([("/".into(), Perm::Allow)]),
        ..Default::default()
    })
    .expect("session setup");
    let ctl = ioctl::open(&s.root.join(".yolofs")).expect("open control fd");
    let target = ioctl::open_rule_target(s.mnt_path("subdir")).expect("open rule target");

    // kernel_write observes RLIMIT_FSIZE. Temporarily setting the soft limit
    // below the existing journal length forces the C append to return EFBIG.
    let result = unsafe {
        let mut old_limit: libc::rlimit = std::mem::zeroed();
        assert_eq!(libc::getrlimit(libc::RLIMIT_FSIZE, &mut old_limit), 0);
        let old_handler = libc::signal(libc::SIGXFSZ, libc::SIG_IGN);
        let blocked = libc::rlimit {
            rlim_cur: 0,
            rlim_max: old_limit.rlim_max,
        };
        assert_eq!(libc::setrlimit(libc::RLIMIT_FSIZE, &blocked), 0);
        let result = ioctl::set_rule_journaled(&ctl, &target, Perm::Deny.to_ioctl());
        assert_eq!(libc::setrlimit(libc::RLIMIT_FSIZE, &old_limit), 0);
        libc::signal(libc::SIGXFSZ, old_handler);
        result
    };

    assert!(
        result.is_err(),
        "forced C append failure must fail RULE_SET"
    );
    assert_eq!(
        ioctl::resolve_rule(&ctl, &target).expect("resolve unchanged policy"),
        Perm::Allow.to_ioctl(),
        "failed C append must not publish the deny rule"
    );
    assert!(
        fs::read_to_string(s.mnt_path("subdir/deep.txt")).is_ok(),
        "the old allow policy must remain enforced"
    );
    assert!(
        notes(&journal(&s))
            .iter()
            .all(|note| !matches!(note, Note::Configure { .. })),
        "a failed append must not leave a valid C record"
    );
}

#[test]
fn configure_is_not_made_unreachable_by_travel() {
    let s = YoloSession::new_with_config(Config {
        rules: BTreeMap::from([("/".into(), Perm::Allow)]),
        ..Default::default()
    })
    .expect("session setup");

    s.cli(&["snapshot", "one"]).expect("snapshot one");
    s.cli(&["rule", "deny", "subdir"])
        .expect("install deny rule");
    let _ = fs::read_to_string(s.mnt_path("subdir/deep.txt"));
    s.cli(&["snapshot", "two"]).expect("snapshot two");
    s.cli(&["rule", "read-only", "subdir"])
        .expect("change live policy before travel");
    s.cli(&["travel", "1"]).expect("travel to one");

    let output = s.cli(&["audit", "all"]).expect("audit all");
    let configure = output
        .lines()
        .find(|line| line.contains("configured") && line.contains("subdir"))
        .expect("C line");
    assert!(
        !configure.contains("unreachable"),
        "C must survive filesystem travel: {configure}"
    );
    let denied = output
        .lines()
        .find(|line| line.contains("denied") && line.contains("deep.txt"))
        .expect("G line");
    assert!(
        denied.contains("unreachable"),
        "G should follow its filesystem segment: {denied}"
    );
    assert!(
        fs::write(s.mnt_path("subdir/deep.txt"), "blocked\n").is_err(),
        "travel must not revert the live read-only policy"
    );

    let each = s
        .cli(&["review", "--each", "all"])
        .expect("review --each all");
    assert!(
        each.contains("configured") && each.contains("subdir"),
        "review --each must retain C from a dead filesystem segment: {each}"
    );
    assert!(
        each.contains("travel 3") && each.contains("→ 1"),
        "a travel-sealed C stanza needs a travel heading: {each}"
    );
}

#[test]
fn directly_allowed_accesses_do_not_emit_gate() {
    let s = YoloSession::new_with_config(Config {
        rules: BTreeMap::from([("/".into(), Perm::Allow)]),
        ..Default::default()
    })
    .expect("session setup");

    fs::read_to_string(s.mnt_path("hello.txt")).expect("direct read allow");
    fs::write(s.mnt_path("hello.txt"), "allowed\n").expect("direct write allow");
    assert!(
        notes(&journal(&s)).is_empty(),
        "directly allowed accesses must not emit G"
    );
}

/// A parent-gated mutate records the attempted child path in G.
#[test]
fn mutate_gate_records_child_target() {
    let s = YoloSession::new_with_config(Config {
        rules: BTreeMap::from([("/".into(), Perm::Allow)]),
        ..Default::default()
    })
    .expect("session setup");
    s.cli(&["rule", "deny", "subdir"])
        .expect("install deny rule");

    let _ = fs::write(s.mnt_path("subdir/new.txt"), "x");

    let j = journal(&s);
    let ns = notes(&j);
    assert!(
        ns.iter().any(|n| matches!(
            n,
            Note::Gate { path, op: Op::Write, result: GateResult::DirectDeny }
                if path.ends_with("/subdir/new.txt")
        )),
        "expected mutate G with child target, got: {ns:?}"
    );
}

/// A write-ask mutate denied with no daemon emits one asked-no G.
#[test]
fn write_ask_mutate_deny_records_one_gate() {
    let s = YoloSession::new_with_config(Config {
        rules: BTreeMap::from([("/".into(), Perm::WriteAsk)]),
        ..Default::default()
    })
    .expect("session setup");

    // Creating a file asks on the parent (write) and defaults to deny.
    let _ = fs::write(s.mnt_path("subdir/new.txt"), "x");

    let j = journal(&s);
    let ns = notes(&j);
    assert!(
        ns.iter().any(|n| matches!(
            n,
            Note::Gate {
                path,
                op: Op::Write,
                result: GateResult::AskDeny,
            } if path.ends_with("/subdir/new.txt")
        )),
        "expected G(write, n) for the mutate, got: {ns:?}"
    );
    assert!(
        !ns.iter().any(|n| matches!(
            n,
            Note::Gate {
                result: GateResult::DirectDeny,
                ..
            }
        )),
        "write-ask mutate deny must not emit direct-deny G, got: {ns:?}"
    );
}

/// G records surface in `yolo review` (as observed-but-not-staged accesses)
/// but never in `yolo review --diff`, which shows staged content only. Repeated
/// identical gates remain distinct audit events.
#[test]
fn gate_records_shown_in_status_hidden_in_diff() {
    use crate::helpers::YOLO_BIN;
    let s = deny_session();

    for _ in 0..3 {
        let _ = fs::read_to_string(s.mnt_path("hello.txt"));
    }
    // Sanity: G records did get emitted.
    let j = journal(&s);
    assert!(!notes(&j).is_empty(), "expected G records to be emitted");

    let status = std::process::Command::new(YOLO_BIN)
        .args(["review"])
        .current_dir(&s.root)
        .env("NO_COLOR", "1")
        .output()
        .expect("yolo review");
    let stdout = String::from_utf8_lossy(&status.stdout);
    assert!(
        stdout.contains("denied") && stdout.contains("hello.txt"),
        "yolo review should surface the G record: {stdout}"
    );
    // All three identical denied reads remain visible.
    assert_eq!(
        stdout.matches("denied").count(),
        3,
        "review should preserve repeated gate records: {stdout}"
    );

    let diff = std::process::Command::new(YOLO_BIN)
        .args(["review", "--diff"])
        .current_dir(&s.root)
        .env("NO_COLOR", "1")
        .output()
        .expect("yolo review --diff");
    let stdout = String::from_utf8_lossy(&diff.stdout);
    assert!(
        !stdout.contains("denied") && !stdout.contains("hello.txt"),
        "yolo review --diff must not show G records (staged content only): {stdout}"
    );
}

/// Asked G notes also surface in `yolo review`.
#[test]
fn ask_records_shown_in_status() {
    let s = YoloSession::new_with_config(Config {
        rules: BTreeMap::new(),
        ..Default::default()
    })
    .expect("session setup");

    let _ = fs::read_to_string(s.mnt_path("hello.txt"));

    // Sanity: a G note was emitted.
    assert!(
        !notes(&journal(&s)).is_empty(),
        "expected a G note to be emitted"
    );

    let out = s.cli(&["review"]).expect("yolo review");
    assert!(
        out.contains("ask") && out.contains("hello.txt"),
        "yolo review should surface the G note: {out}"
    );
}

// ── Asked G results: daemon decision → journal round-trip ───────────
//
// `ask_record_emitted_on_no_daemon` above covers the *no-daemon default*
// path (decision = deny). These tests confirm the inverse: when a live
// `yolo watch` daemon answers an ask interactively, the decision it gives
// (allow / deny) is exactly what the kernel records in the
// G result. This ties the userspace daemon (cli/test_watch.rs) to the journal
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

/// Spawn `yolo watch`, block until it has claimed the daemon slot and is
/// waiting on the ioctl read, pre-fill its stdin with `input` (one decision
/// per line), and return the child so the caller can trigger the ask and then
/// stop the daemon.
fn spawn_watch_with_input(s: &YoloSession, input: &str) -> std::process::Child {
    use crate::helpers::YOLO_BIN;
    use std::io::{BufRead, BufReader, Write};
    use std::process::{Command, Stdio};
    use std::sync::mpsc;
    use std::time::Duration;

    let mut watch = Command::new(YOLO_BIN)
        .args(["watch"])
        .current_dir(&s.root)
        .env("NO_COLOR", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn watch");

    // Drain stderr on a background thread (so the pipe never blocks the
    // daemon) and signal once it prints its readiness line — at which point it
    // has claimed the daemon slot and is blocked on the ioctl read. The thread
    // exits when the child dies and closes its stderr.
    let err = watch.stderr.take().expect("watch stderr piped");
    let (ready_tx, ready_rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut r = BufReader::new(err);
        let mut line = String::new();
        let mut ready = Some(ready_tx);
        loop {
            line.clear();
            match r.read_line(&mut line) {
                Ok(0) | Err(_) => break,
                Ok(_) => {
                    if line.contains("watching for permission requests") {
                        if let Some(tx) = ready.take() {
                            let _ = tx.send(());
                        }
                    }
                }
            }
        }
    });
    ready_rx
        .recv_timeout(Duration::from_secs(10))
        .expect("yolo watch never became ready");

    watch
        .stdin
        .as_mut()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    watch
}

/// Assert the journal contains an asked G for `suffix` with the given op and
/// resolved decision.
fn assert_ask_note(j: &Journal, suffix: &str, op: Op, decision: Decision) {
    let ns = notes(j);
    let result = match decision {
        Decision::Allow => GateResult::AskAllow,
        Decision::Deny => GateResult::AskDeny,
    };
    assert!(
        ns.iter().any(|n| matches!(
            n,
            Note::Gate { path, op: o, result: r }
                if path.ends_with(suffix) && *o == op && *r == result
        )),
        "expected G note {suffix} ({op:?}) -> {result:?}, got: {ns:?}"
    );
}

/// Daemon answers allow, so read succeeds and G records asked-allow.
#[test]
fn daemon_allow_records_ask_note_allow() {
    let s = session_with_ask_file();
    let mut watch = spawn_watch_with_input(&s, "y\n");

    let content = fs::read_to_string(s.mnt_path("hello.txt"));

    watch.kill().ok();
    let _ = watch.wait();

    assert_eq!(
        content.expect("read should succeed after daemon allow"),
        "base content\n"
    );
    assert_ask_note(&journal(&s), "/hello.txt", Op::Read, Decision::Allow);
}

/// Daemon answers deny, so read fails and G records asked-deny.
#[test]
fn daemon_deny_records_ask_note_deny() {
    let s = session_with_ask_file();
    let mut watch = spawn_watch_with_input(&s, "d\n");

    let result = fs::read_to_string(s.mnt_path("hello.txt"));

    watch.kill().ok();
    let _ = watch.wait();

    assert!(result.is_err(), "read should fail after daemon deny");
    assert_ask_note(&journal(&s), "/hello.txt", Op::Read, Decision::Deny);
}

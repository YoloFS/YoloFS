//! Verify the kernel writes B (Blocked) journal records for accesses denied
//! by yolofs rules, and that these records are observational — they do not
//! set the dirty flag and they don't contribute to the dir tree.

use crate::helpers::YoloSession;
use crate::internals::helpers::{actions, blocks, journal};
use std::collections::BTreeMap;
use std::fs;
use yolofs::config::{Config, Perm};
use yolofs::journal::Note;

/// Build a session where the entire mount denies access.
fn deny_session() -> YoloSession {
    YoloSession::new_with_config(Config {
        ask_default: Some(Perm::Deny),
        rules: BTreeMap::from([("/".into(), Perm::Deny)]),
        ..Default::default()
    })
    .expect("session setup")
}

/// Build a session where the entire mount is read-only.
fn ro_session() -> YoloSession {
    YoloSession::new_with_config(Config {
        ask_default: Some(Perm::Deny),
        rules: BTreeMap::from([("/".into(), Perm::Read)]),
        ..Default::default()
    })
    .expect("session setup")
}

/// Block record format: B\0<path>\n (exactly 2 fields).
#[test]
fn block_record_format_is_minimal() {
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
            2,
            "B record should be (B, path), got {} fields: {:?}",
            fields.len(),
            fields
                .iter()
                .map(|f| String::from_utf8_lossy(f))
                .collect::<Vec<_>>()
        );
        assert_eq!(fields[0], b"B");
        assert!(!fields[1].is_empty(), "path field must be non-empty");
    }
}

/// A denied read of a regular file produces a B record at the *target* path.
#[test]
fn denied_read_emits_block_for_file_path() {
    let s = deny_session();

    let _ = fs::read_to_string(s.mnt_path("hello.txt"));

    let j = journal(&s);
    let bs = blocks(&j);
    assert!(
        bs.iter()
            .any(|n| matches!(n, Note::Block { path } if path.ends_with("/hello.txt"))),
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
    let bs = blocks(&j);
    assert!(
        bs.iter()
            .any(|n| matches!(n, Note::Block { path } if path.ends_with("/hello.txt"))),
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
    let bs = blocks(&j);
    assert!(
        bs.iter()
            .any(|n| matches!(n, Note::Block { path } if path.ends_with("/new_file.txt"))),
        "expected B for new_file.txt (child), got: {:?}",
        bs
    );
    // The mutate-denial path must record the *child* target, never just
    // the parent directory. Confirm no record points at the parent only.
    assert!(
        !bs.iter().any(|n| matches!(n, Note::Block { path }
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
    let bs = blocks(&j);
    assert!(
        bs.iter()
            .any(|n| matches!(n, Note::Block { path } if path.ends_with("/hello.txt"))),
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
    let bs = blocks(&j);
    assert!(!bs.is_empty(), "expected B records");
    let acts = actions(&j);
    assert!(
        acts.is_empty(),
        "denied reads must not produce S/D/R actions, got: {:?}",
        acts
    );
}

/// B records do not set sbi->dirty: an auto-snapshot (MARK_IF_CHANGED)
/// must be skipped if only B writes happened since the last meta.
///
/// We check this indirectly via `yolo exec`: a denied-read command produces
/// only B records, and the kernel's MARK_IF_CHANGED auto-snapshot should
/// be skipped, leaving the journal with no M record.
#[test]
fn block_writes_do_not_set_dirty() {
    use crate::helpers::YOLO_BIN;
    let s = YoloSession::new_with_config(Config {
        ask_default: Some(Perm::Deny),
        rules: BTreeMap::from([("/".into(), Perm::Deny)]),
        snapshot: true,
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
    let bs = blocks(&j);
    assert!(
        !bs.is_empty(),
        "expected B records from the denied cat invocation"
    );

    // Phantom meta is index 0; if MARK_IF_CHANGED skipped correctly, no
    // real M record should exist.  metas.len() == 1 means only the phantom.
    assert_eq!(
        j.metas.len(),
        1,
        "MARK_IF_CHANGED should skip auto-snapshot when only B records were written; metas={:?}",
        j.metas.iter().collect::<Vec<_>>()
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
    rules.insert(
        root.join("locked.txt").to_string_lossy().into_owned(),
        Perm::Deny,
    );
    let config = Config {
        permission: true,
        ask_default: Some(Perm::Allow),
        snapshot: true,
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
    let bs = blocks(&j);
    let acts = actions(&j);
    assert!(!bs.is_empty(), "expected B from denied locked.txt read");
    assert!(
        !acts.is_empty(),
        "expected at least one S record from /new.txt write"
    );
    // Auto-snapshot should fire because dirty was set by the S record.
    assert!(
        j.metas.len() >= 2,
        "expected auto-snapshot when mutations occur alongside blocks; metas={:?}",
        j.metas.iter().collect::<Vec<_>>()
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
    let config = Config {
        permission: true,
        ask_default: Some(Perm::Allow),
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
    let bs = blocks(&j);
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
    // No daemon connected; ask_default=deny applies immediately when an
    // unruled file is opened. The deny decision is made inside
    // `yolo_check_dentry_perm` and surfaces from `yolo_open`, not from
    // `yolo_permission` (which returned 0 for the ASK perm).
    let s = YoloSession::new_with_config(Config {
        ask_default: Some(Perm::Deny),
        rules: BTreeMap::new(),
        ..Default::default()
    })
    .expect("session setup");

    let _ = fs::read_to_string(s.mnt_path("hello.txt"));

    let j = journal(&s);
    let bs = blocks(&j);
    assert!(
        bs.iter()
            .any(|n| matches!(n, Note::Block { path } if path.ends_with("/hello.txt"))),
        "expected B for ask-resolved-deny on hello.txt, got: {:?}",
        bs
    );
}

/// B records must be invisible to `yolo status` and `yolo diff` — they are
/// observational and contribute no staged change.
#[test]
fn block_records_invisible_in_status_and_diff() {
    use crate::helpers::YOLO_BIN;
    let s = deny_session();

    for _ in 0..3 {
        let _ = fs::read_to_string(s.mnt_path("hello.txt"));
    }
    // Sanity: B records did get emitted.
    let j = journal(&s);
    assert!(!blocks(&j).is_empty(), "expected B records to be emitted");

    let status = std::process::Command::new(YOLO_BIN)
        .args(["status"])
        .current_dir(&s.root)
        .env("NO_COLOR", "1")
        .output()
        .expect("yolo status");
    let stdout = String::from_utf8_lossy(&status.stdout);
    assert!(
        !stdout.contains("blocked") && !stdout.contains("/hello.txt"),
        "yolo status leaked B record info: {stdout}"
    );

    let diff = std::process::Command::new(YOLO_BIN)
        .args(["diff"])
        .current_dir(&s.root)
        .env("NO_COLOR", "1")
        .output()
        .expect("yolo diff");
    let stdout = String::from_utf8_lossy(&diff.stdout);
    assert!(
        !stdout.contains("blocked") && !stdout.contains("/hello.txt"),
        "yolo diff leaked B record info: {stdout}"
    );
}

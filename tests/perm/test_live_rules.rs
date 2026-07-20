use crate::helpers::{YOLO_BIN, YoloSession};
use std::collections::BTreeMap;
use std::fs;
use yolofs::config::Config;
use yolofs::journal::{GateResult, Journal, Note, Record};
use yolofs::perm::Perm;

// ── Newly created files respect permissions ──

/// A file created under yolofs should still be subject to perm gating
/// when reopened — every check resolves live by walking up the dentry chain.
#[test]
fn newly_created_file_checked_on_reopen() {
    let s = YoloSession::new_with_config(Config {
        rules: BTreeMap::from([("/".into(), Perm::Allow)]),
        ..Default::default()
    })
    .expect("session setup");

    // Create a file (dir op, bypasses perm).
    fs::write(s.mnt_path("newfile.txt"), "hello").expect("create should succeed");

    // Now change rules to deny and re-read.
    s.cli(&["unmount"]).unwrap();
    Config {
        rules: BTreeMap::from([("/".into(), Perm::Deny)]),
        ..Default::default()
    }
    .save(&s.root.join("yolofs.toml"))
    .unwrap();
    std::process::Command::new(YOLO_BIN)
        .arg("mount")
        .current_dir(&s.root)
        .env("NO_COLOR", "1")
        .output()
        .expect("remount");

    let result = fs::read_to_string(s.mnt_path("hello.txt"));
    assert!(
        result.is_err(),
        "read should be denied after rule change to deny"
    );
}

// ── Rule change via live ioctl ──

/// Changing rules at runtime via `yolofs rule <verb>` should take effect
/// on subsequent opens (each check walks up live, so no invalidation needed).
#[test]
fn live_rule_change_takes_effect() {
    let s = YoloSession::new_with_config(Config {
        rules: BTreeMap::from([("/".into(), Perm::Deny)]),
        ..Default::default()
    })
    .expect("session setup");

    let result = fs::read_to_string(s.mnt_path("hello.txt"));
    assert!(result.is_err(), "read should fail under deny");

    // `rule <verb>` takes a host path and resolves it through the mount internally.
    s.cli(&["rule", "allow", &s.root.display().to_string()])
        .unwrap();

    let content = fs::read_to_string(s.mnt_path("hello.txt"))
        .expect("read should succeed after a live rule change");
    assert_eq!(content, "base content\n");
}

/// Removing a rule at runtime should re-gate access.
#[test]
fn live_rule_remove_reapplies_gating() {
    let s = YoloSession::new_with_config(Config {
        rules: BTreeMap::new(),
        ..Default::default()
    })
    .expect("session setup");

    s.cli(&["rule", "allow", &s.root.display().to_string()])
        .unwrap();
    fs::read_to_string(s.mnt_path("hello.txt")).expect("read should succeed with allow rule");

    s.cli(&["rule", "unset", &s.root.display().to_string()])
        .unwrap();
    let result = fs::read_to_string(s.mnt_path("hello.txt"));
    assert!(result.is_err(), "read should fail after rule removal");
}

/// `yolo rule resolve` must report the same permission the kernel enforces —
/// it queries the kernel (YOLO_IOC_RULE_RESOLVE), not a userspace re-derivation.
#[test]
fn rule_resolve_matches_enforcement() {
    let s = YoloSession::new_with_config(Config {
        rules: BTreeMap::new(),
        ..Default::default()
    })
    .expect("session setup");

    let root = s.root.display().to_string();
    s.cli(&["rule", "read-only", &root]).unwrap();

    // Explicit rule on the root resolves to `read-only`.
    let (ok, out, err) = s.cli_output(&["rule", "resolve", &root]).unwrap();
    assert!(ok, "resolve failed: {err}");
    assert!(
        out.contains("read-only"),
        "root should resolve to read-only, got: {out}"
    );

    // A child path inherits `read-only`.
    let child = s.root.join("hello.txt").display().to_string();
    let (ok, out, _) = s.cli_output(&["rule", "resolve", &child]).unwrap();
    assert!(
        ok && out.contains("read-only"),
        "child should inherit read-only, got: {out}"
    );

    // Parity with enforcement: read is allowed, write is denied under `read-only`.
    assert!(
        fs::read_to_string(s.mnt_path("hello.txt")).is_ok(),
        "read should be allowed under a read-only rule"
    );
    assert!(
        fs::write(s.mnt_path("hello.txt"), "x").is_err(),
        "write should be denied under a read-only rule"
    );
}

/// `yolo rule write-ask` is a public rule verb: reads pass, writes ask and are
/// denied when no daemon is connected.
#[test]
fn rule_write_ask_resolve_matches_enforcement() {
    let s = YoloSession::new_with_config(Config {
        rules: BTreeMap::new(),
        ..Default::default()
    })
    .expect("session setup");

    let root = s.root.display().to_string();
    s.cli(&["rule", "write-ask", &root]).unwrap();

    let (ok, out, err) = s.cli_output(&["rule", "resolve", &root]).unwrap();
    assert!(ok, "resolve failed: {err}");
    assert!(
        out.contains("write-ask"),
        "root should resolve to write-ask, got: {out}"
    );

    fs::read_to_string(s.mnt_path("hello.txt"))
        .expect("read should be allowed under a write-ask rule");
    assert!(
        fs::write(s.mnt_path("hello.txt"), "x").is_err(),
        "write should ask and be denied with no daemon"
    );
}

// ── Rename across permission boundaries ──

/// Renaming a file from an allowed dir into a denied dir is rejected: the
/// mutate check gates on the destination parent's access (deny), and the
/// source file stays readable in the allowed dir.
#[test]
fn rename_across_permission_boundary() {
    let s = YoloSession::new_with_config(Config {
        permission: false,
        rules: BTreeMap::new(),
        ..Default::default()
    })
    .expect("session setup");

    // Create base files directly.
    fs::create_dir_all(s.root.join("allowed")).expect("mkdir allowed");
    fs::create_dir_all(s.root.join("denied")).expect("mkdir denied");
    fs::write(s.root.join("allowed/file.txt"), "content\n").expect("create file");

    s.cli(&["unmount"]).unwrap();
    Config {
        rules: BTreeMap::from([
            (s.root.join("allowed").display().to_string(), Perm::Allow),
            (s.root.join("denied").display().to_string(), Perm::Deny),
        ]),
        ..Default::default()
    }
    .save(&s.root.join("yolofs.toml"))
    .unwrap();
    std::process::Command::new(YOLO_BIN)
        .arg("mount")
        .current_dir(&s.root)
        .env("NO_COLOR", "1")
        .output()
        .expect("remount");

    // Can read the file in the allowed dir.
    fs::read_to_string(s.mnt_path("allowed/file.txt"))
        .expect("reading file in allowed dir should succeed");

    // Rename into a denied directory should fail (destination is deny).
    let result = fs::rename(
        s.mnt_path("allowed/file.txt"),
        s.mnt_path("denied/file.txt"),
    );
    assert!(result.is_err(), "rename into denied dir should fail");

    // File should still be readable in the allowed directory.
    let content = fs::read_to_string(s.mnt_path("allowed/file.txt"))
        .expect("file should still be in allowed dir");
    assert_eq!(content, "content\n");
}

// ── fd-based rule targets ──

/// Rule targets reach the kernel as O_PATH fds, so a rule path is not capped
/// by the kernel's YOLO_PATH_MAX (256 bytes) — which the through-mount form
/// (`<mnt>/<abs-path>`) used to overflow easily on deep trees.
#[test]
fn rule_on_path_longer_than_yolo_path_max() {
    let s = YoloSession::new_with_config(Config {
        rules: BTreeMap::from([("/".into(), Perm::Allow)]),
        ..Default::default()
    })
    .expect("session setup");

    // Build a directory whose absolute path alone exceeds 256 bytes.
    let mut rel = std::path::PathBuf::new();
    for _ in 0..5 {
        rel = rel.join("d".repeat(60));
    }
    let deep = s.root.join(&rel);
    fs::create_dir_all(&deep).expect("mkdir deep");
    fs::write(deep.join("file.txt"), "deep\n").expect("seed deep file");
    let deep_str = deep.display().to_string();
    assert!(
        deep_str.len() > 256,
        "test premise: rule path must exceed YOLO_PATH_MAX, got {}",
        deep_str.len()
    );

    s.cli(&["rule", "deny", &deep_str])
        .expect("setting a rule on a >256-byte path should succeed");

    let through = s.mnt_path(&rel.join("file.txt").display().to_string());
    assert!(
        fs::read_to_string(&through).is_err(),
        "deny rule on the deep path must be enforced"
    );

    let journal = Journal::read(&s.root.join(".yolofs")).expect("read journal");
    assert!(
        journal
            .segments
            .iter()
            .flat_map(|segment| &segment.records)
            .any(|record| matches!(
                record,
                Record::Note(Note::Gate {
                    path,
                    result: GateResult::DirectDeny,
                    ..
                }) if path.ends_with("/file.txt") && path.len() > 256
            )),
        "a static denial on a long path must still emit G"
    );
}

use crate::helpers::{YOLO_BIN, YoloSession};
use std::collections::BTreeMap;
use yolofs::config::Config;
use yolofs::perm::Perm;

#[test]
fn mount_and_unmount() {
    let session = YoloSession::new().expect("session setup");

    // Verify mount point exists and is accessible
    assert!(session.mnt.exists(), "mount point exists");
    assert!(
        session.mnt.join("tmp").exists(),
        "root fs visible through mount"
    );

    // Verify test files visible through mount
    assert!(session.mnt_path("hello.txt").exists());
    assert!(session.mnt_path("subdir/deep.txt").exists());

    drop(session); // triggers unmount
}

#[test]
fn mount_creates_layout() {
    let session = YoloSession::new().expect("session setup");

    assert!(session.root.join(".yolofs").exists());
    assert!(session.root.join(".yolofs/inodes").exists());
    assert!(session.mnt.exists());

    drop(session);
}

#[test]
fn remount_picks_up_new_rules() {
    let session = YoloSession::new_with_config(Config {
        permission: false,
        ..Default::default()
    })
    .expect("session setup");

    // Initially no rules — mount should work
    let (ok, _, stderr) = session.cli_output(&["mount"]).unwrap();
    assert!(ok, "mount should succeed: {stderr}");

    // Write config with rules, remount
    Config {
        permission: false,
        rules: BTreeMap::from([("/etc".into(), Perm::ReadOnly)]),
        ..Default::default()
    }
    .save(&session.root.join("yolofs.toml"))
    .unwrap();

    let (ok, _, stderr) = session.cli_output(&["remount"]).unwrap();
    assert!(ok, "remount should succeed: {stderr}");
    assert!(
        stderr.contains("applying 1 rule"),
        "remount should apply rules: {stderr}"
    );
}

#[test]
fn unknown_subcommand_shows_help() {
    let output = std::process::Command::new(YOLO_BIN)
        .arg("notarealcommand")
        .output()
        .expect("running yolofs");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let combined = format!("{stdout}{stderr}");

    assert!(
        combined.contains("Usage") || combined.contains("usage"),
        "should show help for unknown subcommand: {combined}"
    );
}

// ---------------------------------------------------------------------------
// Mount error-path tests
// ---------------------------------------------------------------------------

/// Without an yolofs.toml, mount should still succeed using default options.
#[test]
fn mount_no_config_uses_defaults() {
    let tmp = tempfile::tempdir().expect("creating temp dir");

    let output = std::process::Command::new(YOLO_BIN)
        .arg("mount")
        .current_dir(tmp.path())
        .env("NO_COLOR", "1")
        .output()
        .expect("running yolofs mount");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "yolofs mount should succeed even without yolofs.toml (uses defaults): {stderr}"
    );

    // Clean up — unmount the session we just created
    let _ = std::process::Command::new(YOLO_BIN)
        .args(["unmount", "--force"])
        .current_dir(tmp.path())
        .env("NO_COLOR", "1")
        .output();
}

/// Invalid yolofs.toml should cause `yolofs mount` to fail (at the apply_rules step).
#[test]
fn mount_invalid_config_fails() {
    let tmp = tempfile::tempdir().expect("creating temp dir");
    std::fs::write(tmp.path().join("yolofs.toml"), "{{invalid toml")
        .expect("writing invalid config");

    let output = std::process::Command::new(YOLO_BIN)
        .arg("mount")
        .current_dir(tmp.path())
        .env("NO_COLOR", "1")
        .output()
        .expect("running yolofs mount");

    assert!(
        !output.status.success(),
        "yolofs mount should fail with an invalid yolofs.toml"
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("parse")
            || stderr.contains("invalid")
            || stderr.contains("toml")
            || stderr.contains("expected"),
        "stderr should mention parse/config error: {stderr}"
    );

    // Clean up — mount may have partially succeeded before apply_rules failed
    let _ = std::process::Command::new(YOLO_BIN)
        .args(["unmount", "--force"])
        .current_dir(tmp.path())
        .env("NO_COLOR", "1")
        .output();
}

#[test]
fn mount_nonexistent_dir_fails() {
    let bad_path = std::path::PathBuf::from("/tmp/yolo_nonexistent_dir_that_does_not_exist");
    // Ensure it really does not exist.
    assert!(!bad_path.exists(), "sanity: path should not exist");

    let result = std::process::Command::new(YOLO_BIN)
        .arg("mount")
        .current_dir(&bad_path)
        .env("NO_COLOR", "1")
        .output();

    // Setting current_dir to a nonexistent path causes Command::output() itself
    // to return an Err (on Linux: "No such file or directory").
    assert!(
        result.is_err(),
        "Command::output() should fail when current_dir does not exist"
    );
}

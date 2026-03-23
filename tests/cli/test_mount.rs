use crate::helpers::{AGFS_BIN, AgfsSession};
use agfs::config::{Config, Perm};
use std::collections::BTreeMap;

#[test]
fn mount_and_unmount() {
    let Some(session) = AgfsSession::new().expect("session setup") else { return };
    // Verify mount point exists and is accessible
    assert!(session.mnt.exists(), "mount point exists");
    assert!(
        session.mnt.join("tmp").exists(),
        "root fs visible through mount"
    );

    // Verify test files visible through mount
    assert!(session.mnt_path("hello.txt").exists());
    assert!(session.mnt_path("subdir/deep.txt").exists());
}

#[test]
fn mount_creates_layout() {
    let Some(session) = AgfsSession::new().expect("session setup") else { return };
    assert!(session.root.join(".agfs").exists());
    assert!(session.root.join(".agfs/inodes").exists());
    assert!(session.mnt.exists());
}

#[test]
fn remount_picks_up_new_rules() {
    let Some(session) = AgfsSession::new_with_config(Config {
        permission: false,
        ..Default::default()
    })
    .expect("session setup") else { return };

    // Write config with rules, remount — both from host
    Config {
        permission: false,
        rules: BTreeMap::from([("/etc".into(), Perm::AllowRo)]),
        ..Default::default()
    }
    .save(&session.root.join("agfs.toml"))
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
    let output = std::process::Command::new(AGFS_BIN)
        .arg("notarealcommand")
        .output()
        .expect("running agfs");

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

/// Without an agfs.toml, mount should still succeed using default options.
#[test]
fn mount_no_config_uses_defaults() {
    let tmp = tempfile::tempdir().expect("creating temp dir");

    let output = std::process::Command::new(AGFS_BIN)
        .arg("mount")
        .current_dir(tmp.path())
        .env("NO_COLOR", "1")
        .output()
        .expect("running agfs mount");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "agfs mount should succeed even without agfs.toml (uses defaults): {stderr}"
    );

    // Clean up — unmount the session we just created
    let _ = std::process::Command::new(AGFS_BIN)
        .args(["unmount", "--force"])
        .current_dir(tmp.path())
        .env("NO_COLOR", "1")
        .output();
}

/// Invalid agfs.toml should cause `agfs mount` to fail (at the apply_rules step).
#[test]
fn mount_invalid_config_fails() {
    let tmp = tempfile::tempdir().expect("creating temp dir");
    std::fs::write(tmp.path().join("agfs.toml"), "{{invalid toml").expect("writing invalid config");

    let output = std::process::Command::new(AGFS_BIN)
        .arg("mount")
        .current_dir(tmp.path())
        .env("NO_COLOR", "1")
        .output()
        .expect("running agfs mount");

    assert!(
        !output.status.success(),
        "agfs mount should fail with an invalid agfs.toml"
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
    let _ = std::process::Command::new(AGFS_BIN)
        .args(["unmount", "--force"])
        .current_dir(tmp.path())
        .env("NO_COLOR", "1")
        .output();
}

#[test]
fn mount_nonexistent_dir_fails() {
    let bad_path = std::path::PathBuf::from("/tmp/agfs_nonexistent_dir_that_does_not_exist");
    // Ensure it really does not exist.
    assert!(!bad_path.exists(), "sanity: path should not exist");

    let result = std::process::Command::new(AGFS_BIN)
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

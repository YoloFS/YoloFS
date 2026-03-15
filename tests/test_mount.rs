use crate::helpers::{AGFS_BIN, AgfsSession};
use agfs::config::{Config, MountConfig, Perm};
use std::collections::BTreeMap;

#[test]
fn mount_and_unmount() {
    let session = AgfsSession::new().expect("session setup");

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
    let session = AgfsSession::new().expect("session setup");

    assert!(session.root.join(".agfs").exists());
    assert!(session.root.join(".agfs/staging").exists());
    assert!(session.mnt.exists());

    drop(session);
}

#[test]
fn remount_picks_up_new_rules() {
    let session = AgfsSession::new_with_config(Config {
        mount: MountConfig {
            noperm: true,
            ..Default::default()
        },
        ..Default::default()
    })
    .expect("session setup");

    // Initially no rules — mount should work
    let (ok, _, stderr) = session.cli_output(&["mount"]).unwrap();
    assert!(ok, "mount should succeed: {stderr}");

    // Write config with rules, remount
    Config {
        mount: MountConfig {
            noperm: true,
            ..Default::default()
        },
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

use crate::helpers::AGFS_BIN;
use crate::helpers::AgfsSession;
use agfs::config::{Config, Perm};
use std::collections::BTreeMap;

#[test]
fn apply_rules_shows_results() {
    let session = AgfsSession::new().expect("session setup");

    // Unmount, write custom rules, remount
    session.cli(&["unmount"]).unwrap();
    Config {
        permission: false,
        rules: BTreeMap::from([
            ("/etc".into(), Perm::AllowRo),
            ("/usr".into(), Perm::AllowRx),
        ]),
        ..Default::default()
    }
    .save(&session.root.join("agfs.toml"))
    .unwrap();

    let output = std::process::Command::new(AGFS_BIN)
        .arg("mount")
        .current_dir(&session.root)
        .env("NO_COLOR", "1")
        .output()
        .expect("running agfs mount");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("applying 2 rule(s)"), "stderr = {stderr}");
    assert!(stderr.contains("/etc = allow-ro"), "stderr = {stderr}");
    assert!(stderr.contains("/usr = allow-rx"), "stderr = {stderr}");
}

#[test]
fn apply_rules_rejects_invalid_toml() {
    let session = AgfsSession::new().expect("session setup");

    session.cli(&["unmount"]).unwrap();
    // Write raw TOML with an invalid perm — typed Config can't represent this
    std::fs::write(
        session.root.join("agfs.toml"),
        "permission = false\n\n[rules]\n\"/etc\" = \"bogus\"\n",
    )
    .unwrap();

    let output = std::process::Command::new(AGFS_BIN)
        .arg("mount")
        .current_dir(&session.root)
        .env("NO_COLOR", "1")
        .output()
        .expect("running agfs mount");

    assert!(
        !output.status.success(),
        "mount should fail with invalid perm in config"
    );
}

#[test]
fn rule_add_persists_offline() {
    let session = AgfsSession::new().expect("session setup");

    session.cli(&["unmount"]).unwrap();
    Config {
        permission: false,
        rules: BTreeMap::new(),
        ..Default::default()
    }
    .save(&session.root.join("agfs.toml"))
    .unwrap();

    let (ok, _, stderr) = session
        .cli_output(&["rule", "add", "/tmp", "allow-rw"])
        .unwrap();
    assert!(ok, "rule add should succeed: {stderr}");

    let content = std::fs::read_to_string(session.root.join("agfs.toml")).unwrap();
    assert!(
        content.contains("allow-rw"),
        "rule should be in agfs.toml: {content}"
    );
}

#[test]
fn tilde_rule_resolves_to_home() {
    let session = AgfsSession::new().expect("session setup");

    // Unmount, write config with ~ rule, remount
    session.cli(&["unmount"]).unwrap();
    std::fs::write(
        session.root.join("agfs.toml"),
        "permission = false\n\n[rules]\n\"~\" = \"allow-rw\"\n",
    )
    .unwrap();

    let output = std::process::Command::new(AGFS_BIN)
        .arg("mount")
        .current_dir(&session.root)
        .env("NO_COLOR", "1")
        .output()
        .expect("running agfs mount");

    let stderr = String::from_utf8_lossy(&output.stderr);
    let home = std::env::var("HOME").unwrap();
    assert!(output.status.success(), "mount should succeed: {stderr}");
    assert!(
        stderr.contains(&home),
        "~ should resolve to {home}: {stderr}"
    );
}

#[test]
fn nonexistent_rule_path_warns() {
    let session = AgfsSession::new().expect("session setup");

    session.cli(&["unmount"]).unwrap();
    std::fs::write(
        session.root.join("agfs.toml"),
        "permission = false\n\n[rules]\n\"/nonexistent_agfs_xyz\" = \"allow-rw\"\n",
    )
    .unwrap();

    let output = std::process::Command::new(AGFS_BIN)
        .arg("mount")
        .current_dir(&session.root)
        .env("NO_COLOR", "1")
        .output()
        .expect("running agfs mount");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "mount should still succeed: {stderr}"
    );
    assert!(
        stderr.contains("does not exist"),
        "should warn about nonexistent path: {stderr}"
    );
}

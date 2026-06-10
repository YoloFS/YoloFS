use crate::helpers::YOLO_BIN;
use crate::helpers::YoloSession;
use std::collections::BTreeMap;
use yolofs::config::Config;
use yolofs::perm::Perm;

#[test]
fn apply_rules_reports_count_and_lists_via_rule() {
    let session = YoloSession::new().expect("session setup");

    // Unmount, write custom rules, remount
    session.cli(&["unmount"]).unwrap();
    Config {
        permission: false,
        rules: BTreeMap::from([
            ("/etc".into(), Perm::WriteAsk),
            ("/usr".into(), Perm::ReadOnly),
        ]),
        ..Default::default()
    }
    .save(&session.root.join("yolofs.toml"))
    .unwrap();

    let output = std::process::Command::new(YOLO_BIN)
        .arg("mount")
        .current_dir(&session.root)
        .env("NO_COLOR", "1")
        .output()
        .expect("running yolofs mount");

    // Mount reports only the count; per-rule lines were dropped to cut noise.
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("applying 2 rules"), "stderr = {stderr}");

    // The rules themselves are inspected via `yolo rule list`.
    let (_ok, stdout, _e) = session.cli_output(&["rule", "list"]).unwrap();
    assert!(stdout.contains("/etc = write-ask"), "rule list: {stdout}");
    assert!(stdout.contains("/usr = read-only"), "rule list: {stdout}");
}

#[test]
fn apply_rules_rejects_invalid_toml() {
    let session = YoloSession::new().expect("session setup");

    session.cli(&["unmount"]).unwrap();
    // Write raw TOML with an invalid perm — typed Config can't represent this
    std::fs::write(
        session.root.join("yolofs.toml"),
        "permission = false\n\n[rules]\n\"/etc\" = \"bogus\"\n",
    )
    .unwrap();

    let output = std::process::Command::new(YOLO_BIN)
        .arg("mount")
        .current_dir(&session.root)
        .env("NO_COLOR", "1")
        .output()
        .expect("running yolofs mount");

    assert!(
        !output.status.success(),
        "mount should fail with invalid perm in config"
    );
}

#[test]
fn rule_set_persists_offline() {
    let session = YoloSession::new().expect("session setup");

    session.cli(&["unmount"]).unwrap();
    Config {
        permission: false,
        rules: BTreeMap::new(),
        ..Default::default()
    }
    .save(&session.root.join("yolofs.toml"))
    .unwrap();

    // With no rules configured, `rule list`'s empty data answer is on stdout.
    let list = session.cli(&["rule", "list"]).unwrap();
    assert!(
        list.contains("(no rules configured)"),
        "empty rule list: {list}"
    );

    let (ok, _, stderr) = session.cli_output(&["rule", "allow", "/tmp"]).unwrap();
    assert!(ok, "rule allow should succeed: {stderr}");
    // Unmounted => the rule is saved, not applied live; the status says which.
    assert!(
        stderr.contains("rule saved") && stderr.contains("takes effect on next mount"),
        "offline rule set should report saved-not-applied: {stderr}"
    );

    let content = std::fs::read_to_string(session.root.join("yolofs.toml")).unwrap();
    assert!(
        content.contains("allow"),
        "rule should be in yolofs.toml: {content}"
    );
}

#[test]
fn nonexistent_rule_path_warns() {
    let session = YoloSession::new().expect("session setup");

    session.cli(&["unmount"]).unwrap();
    std::fs::write(
        session.root.join("yolofs.toml"),
        "permission = false\n\n[rules]\n\"/nonexistent_yolo_xyz\" = \"allow\"\n",
    )
    .unwrap();

    let output = std::process::Command::new(YOLO_BIN)
        .arg("mount")
        .current_dir(&session.root)
        .env("NO_COLOR", "1")
        .output()
        .expect("running yolofs mount");

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

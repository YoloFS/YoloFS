use crate::helpers::AgfsSession;
use crate::helpers::AGFS_BIN;

#[test]
fn apply_rules_shows_results() {
    let session = AgfsSession::new().expect("session setup");

    // Unmount, write custom rules, remount
    session.cli(&["unmount"]).unwrap();
    std::fs::write(
        session.root.join("agfs.toml"),
        "[mount]\nnoperm = true\n\n[rules]\n\"/etc\" = \"allow-ro\"\n\"/usr\" = \"allow-rx\"\n",
    ).unwrap();

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
fn apply_rules_shows_invalid_perm() {
    let session = AgfsSession::new().expect("session setup");

    session.cli(&["unmount"]).unwrap();
    std::fs::write(
        session.root.join("agfs.toml"),
        "[mount]\nnoperm = true\n\n[rules]\n\"/etc\" = \"bogus\"\n",
    ).unwrap();

    let output = std::process::Command::new(AGFS_BIN)
        .arg("mount")
        .current_dir(&session.root)
        .env("NO_COLOR", "1")
        .output()
        .expect("running agfs mount");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("invalid permission"), "stderr = {stderr}");
}

#[test]
fn rule_add_persists_offline() {
    let session = AgfsSession::new().expect("session setup");

    session.cli(&["unmount"]).unwrap();
    std::fs::write(
        session.root.join("agfs.toml"),
        "[mount]\nnoperm = true\n\n[rules]\n",
    ).unwrap();

    let (ok, _, stderr) = session.cli_output(&["rule", "add", "/tmp", "allow-rw"]).unwrap();
    assert!(ok, "rule add should succeed: {stderr}");

    let content = std::fs::read_to_string(session.root.join("agfs.toml")).unwrap();
    assert!(content.contains("allow-rw"), "rule should be in agfs.toml: {content}");
}


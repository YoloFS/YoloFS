use crate::helpers::{AGFS_BIN, AgfsSession};
use agfs::config::{Config, Perm};
use std::collections::BTreeMap;
use std::process::{Command, Stdio};
use std::time::Duration;

/// `agfs watch --allow-all` should answer every ask with allow, so
/// `touch a` inside `agfs exec` must succeed.
///
/// Regression: even with the daemon running and --allow-all set, the
/// kernel never delivers the ask for the new file and the touch fails.
#[test]
fn watch_allow_all_daemon_allows_file_creation_inside_exec() {
    let s = AgfsSession::new_with_config(Config {
        ask_default: Some(Perm::Ask),
        rules: BTreeMap::new(),
        ..Default::default()
    })
    .expect("session setup");

    // Start the daemon before exec so it is already blocked in ioctl read
    // when the kernel raises the ask for the new file.
    let mut watch = std::process::Command::new(AGFS_BIN)
        .args(["watch", "--allow-all"])
        .current_dir(&s.root)
        .env("NO_COLOR", "1")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawning agfs watch --allow-all");

    // Give the daemon time to open the ioctl fd and block on the first read.
    std::thread::sleep(Duration::from_millis(200));

    // touch creates a new file — the kernel must ask the daemon and receive
    // an allow response for this to succeed.
    let code = s.run_in_sandbox(&["touch", "a"]).unwrap_or(1);

    watch.kill().ok();
    let output = watch.wait_with_output().expect("collecting watch output");
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();

    // The daemon must have logged an ask whose path ends with /a.
    let expected_path = format!("{}/a", s.root.display());
    assert!(
        stderr.contains(&expected_path),
        "watch daemon should have received an ask for {expected_path}, got:\n{stderr}"
    );

    assert_eq!(
        code, 0,
        "touch a should succeed when watch --allow-all is running"
    );
}

/// Starting a second watch while one is already running should fail
/// with a clear "already running" message (kernel returns EBUSY).
#[test]
fn second_watch_reports_already_running() {
    let s = AgfsSession::new_with_config(Config {
        ask_default: Some(Perm::Deny),
        rules: BTreeMap::new(),
        ..Default::default()
    })
    .expect("session setup");

    // Start first watch in background.
    let mut watch1 = Command::new(AGFS_BIN)
        .arg("watch")
        .current_dir(&s.root)
        .env("NO_COLOR", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn first watch");

    // Give it time to register with the kernel (ensure_ctl sets has_daemon).
    std::thread::sleep(Duration::from_millis(300));

    // Second watch should fail with "already running".
    let (ok, _, stderr) = s.cli_output(&["watch"]).unwrap();
    assert!(!ok, "second watch should fail");
    assert!(
        stderr.contains("already running"),
        "error should mention 'already running': {stderr}"
    );

    // Clean up.
    let _ = Command::new("kill").arg(watch1.id().to_string()).status();
    let _ = watch1.wait();
}

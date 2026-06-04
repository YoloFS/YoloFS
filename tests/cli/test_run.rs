use crate::helpers::YoloSession;

#[test]
fn run_success_exit_code_zero() {
    let session = YoloSession::new().expect("session setup");

    let code = session.run_in_yolofs(&["true"]).unwrap();
    assert_eq!(code, 0, "successful command should return exit code 0");
}

#[test]
fn run_failure_exit_code_propagated() {
    let session = YoloSession::new().expect("session setup");

    let code = session.run_in_yolofs(&["false"]).unwrap();
    assert_eq!(code, 1, "`false` should return exit code 1");
}

#[test]
fn run_custom_exit_code() {
    let session = YoloSession::new().expect("session setup");

    let code = session.run_in_yolofs(&["sh", "-c", "exit 42"]).unwrap();
    assert_eq!(code, 42, "exit 42 should propagate as exit code 42");
}

#[test]
fn run_command_not_found() {
    let session = YoloSession::new().expect("session setup");

    let code = session.run_in_yolofs(&["nonexistent_cmd_xyz"]).unwrap();
    assert_ne!(
        code, 0,
        "nonexistent command should return non-zero exit code"
    );
}

#[test]
fn run_shell_pipe() {
    let session = YoloSession::new().expect("session setup");

    let chroot_path = session.root.join("hello.txt");
    let code = session
        .run_in_yolofs(&[
            "sh",
            "-c",
            &format!("cat {} | grep base", chroot_path.display()),
        ])
        .unwrap();
    assert_eq!(code, 0, "shell pipe should work");
}

#[test]
fn run_shell_quotes() {
    let session = YoloSession::new().expect("session setup");

    let chroot_path = session.root.join("hello.txt");
    let code = session
        .run_in_yolofs(&[
            "sh",
            "-c",
            &format!("test \"$(cat {})\" = 'base content'", chroot_path.display()),
        ])
        .unwrap();
    assert_eq!(code, 0, "shell quotes should be preserved");
}

#[test]
fn run_reads_file_through_mount() {
    let session = YoloSession::new().expect("session setup");

    // Inside the chroot, paths are relative to the mount root (which IS /)
    // The test file lives at <root>/hello.txt, visible as <root>/hello.txt inside chroot
    let chroot_path = session.root.join("hello.txt");
    let code = session
        .run_in_yolofs(&["cat", chroot_path.to_str().unwrap()])
        .unwrap();
    assert_eq!(
        code, 0,
        "cat should succeed reading a file inside the yolofs"
    );
}

/// Writing to a file via absolute path inside the yolofs should succeed.
#[test]
fn run_write_file_absolute_path() {
    let session = YoloSession::new().expect("session setup");

    let target = session.root.join("exec_output.txt");
    let code = session
        .run_in_yolofs(&["sh", "-c", &format!("echo hello > {}", target.display())])
        .unwrap();
    assert_eq!(code, 0, "writing to absolute path should succeed");
}

/// Writing to a file via relative path inside the yolofs should succeed.
/// The cwd after chroot+chdir is the session root directory.
#[test]
fn run_write_file_relative_path() {
    let session = YoloSession::new().expect("session setup");

    let code = session
        .run_in_yolofs(&["sh", "-c", "echo hello > relative_test.txt"])
        .unwrap();
    assert_eq!(
        code, 0,
        "writing to a relative path inside the yolofs should succeed"
    );
}

#[test]
fn run_env_var_propagated() {
    let session = YoloSession::new().expect("session setup");
    // YOLO_SESSION is always set by exec.rs — verify the yolofs sees it
    let code = session
        .run_in_yolofs(&["sh", "-c", "test -n \"$YOLO_SESSION\""])
        .unwrap();
    assert_eq!(code, 0, "YOLO_SESSION env var should be set in yolofs");
}

#[test]
fn run_stderr_output() {
    let session = YoloSession::new().expect("session setup");
    let code = session
        .run_in_yolofs(&["sh", "-c", "echo err >&2"])
        .unwrap();
    assert_eq!(code, 0, "writing to stderr should succeed");
}

#[test]
fn run_reads_modified_file() {
    let session = YoloSession::new().expect("session setup");
    let target = session.root.join("hello.txt");
    // Modify through mount
    std::fs::write(session.mnt_path("hello.txt"), "modified\n").expect("write");

    // Read inside yolofs — should see modified content
    let code = session
        .run_in_yolofs(&[
            "sh",
            "-c",
            &format!("grep -q modified {}", target.display()),
        ])
        .unwrap();
    assert_eq!(code, 0, "yolofs should see modified file content");
}

#[test]
fn run_multiple_commands_sequentially() {
    let session = YoloSession::new().expect("session setup");
    let target = session.root.join("seq_test.txt");

    let code1 = session
        .run_in_yolofs(&["sh", "-c", &format!("echo first > {}", target.display())])
        .unwrap();
    assert_eq!(code1, 0);

    let code2 = session
        .run_in_yolofs(&["sh", "-c", &format!("grep -q first {}", target.display())])
        .unwrap();
    assert_eq!(code2, 0, "second command should see first command's output");
}

/// Auto-snapshot is skipped when the exec command produces no changes.
#[test]
fn run_no_changes_skips_snapshot() {
    let session = YoloSession::new().expect("session setup");

    let before = session.cli(&["timeline"]).expect("timeline before");
    let before_count = before.matches("snapshot").count();

    // Run a read-only command — no staged changes
    let code = session.run_in_yolofs(&["true"]).unwrap();
    assert_eq!(code, 0);

    let after = session.cli(&["timeline"]).expect("timeline after");
    let after_count = after.matches("snapshot").count();

    assert_eq!(
        before_count, after_count,
        "no-op exec should not create a snapshot.\nbefore:\n{before}\nafter:\n{after}"
    );
}

/// Auto-snapshot is created when the exec command makes changes.
#[test]
fn run_with_changes_creates_snapshot() {
    let session = YoloSession::new().expect("session setup");

    let before = session.cli(&["timeline"]).expect("timeline before");
    let before_count = before.matches("snapshot [").count();

    let target = session.root.join("chk_test.txt");
    let code = session
        .run_in_yolofs(&["sh", "-c", &format!("echo hello > {}", target.display())])
        .unwrap();
    assert_eq!(code, 0);

    let after = session.cli(&["timeline"]).expect("timeline after");
    let after_count = after.matches("snapshot [").count();

    assert!(
        after_count == before_count + 1,
        "exec with changes should create exactly one snapshot.\nbefore:\n{before}\nafter:\n{after}"
    );
}

// ── `yolo -- <cmd>` shorthand: run, then review (vs quiet `exec`) ────

/// `yolo -- <cmd>` runs the command and then prints a status summary of what
/// it changed (the run-and-review path), unlike the quiet `exec`.
#[test]
fn run_shorthand_shows_status() {
    let session = YoloSession::new().expect("session setup");

    let (ok, stdout, _err) = session
        .cli_output(&["--", "sh", "-c", "echo hi > shorthand.txt"])
        .expect("yolo -- cmd");

    assert!(ok, "command should succeed");
    assert!(
        stdout.contains("shorthand.txt"),
        "`yolo -- <cmd>` should print a status summary naming the changed file: {stdout}"
    );
    assert!(
        stdout.contains("staged change"),
        "the summary should mention staged changes: {stdout}"
    );
}

/// `yolo -- <cmd>` reviews what THAT command did. A no-op command run after a
/// real change must report no changes — not echo the previous command's staged
/// change (regression: it used to fall back to the last snapshot's batch).
#[test]
fn run_shorthand_noop_after_change_shows_no_changes() {
    let session = YoloSession::new().expect("session setup");

    // First command stages a change (auto-snapshots).
    session
        .cli_output(&["--", "sh", "-c", "echo hi > made.txt"])
        .expect("first run");

    // Second command changes nothing — its review must say no changes, and must
    // NOT show the previous command's change.
    let (ok, stdout, _err) = session.cli_output(&["--", "true"]).expect("no-op run");
    assert!(ok, "no-op command should succeed");
    assert!(
        stdout.contains("no changes"),
        "a no-op command's review should report no changes: {stdout}"
    );
    assert!(
        !stdout.contains("made.txt"),
        "review must not echo the previous command's change: {stdout}"
    );
}

/// `yolo exec -- <cmd>` is the quiet primitive: it must NOT print a status
/// summary (the snapshot line it does emit goes to stderr).
#[test]
fn exec_stays_quiet_no_status() {
    let session = YoloSession::new().expect("session setup");

    let (ok, stdout, _err) = session
        .cli_output(&["exec", "--", "sh", "-c", "echo hi > quiet.txt"])
        .expect("yolo exec -- cmd");

    assert!(ok, "command should succeed");
    assert!(
        !stdout.contains("staged change"),
        "`yolo exec` should not print a status summary on stdout: {stdout}"
    );
}

/// The shorthand propagates the command's exit code, just like `exec`.
#[test]
fn run_shorthand_propagates_exit_code() {
    let session = YoloSession::new().expect("session setup");

    let code = session
        .cli_exit_code(&["--", "sh", "-c", "exit 42"])
        .unwrap();
    assert_eq!(
        code, 42,
        "`yolo -- <cmd>` should propagate the command's exit code"
    );
}

/// yolo is a host-side tool: every command refuses to run inside the mount
/// (its base-fs operations only work outside). Running it via `yolo exec`
/// chroots it inside, where the top-level guard rejects it.
#[test]
fn run_yolo_inside_mount_is_rejected() {
    let session = YoloSession::new().expect("session setup");

    let (ok, _out, err) = session
        .cli_output(&["exec", "--", "yolo", "review"])
        .expect("running yolo inside the mount");

    assert!(!ok, "yolo inside the mount should fail; stderr={err}");
    assert!(
        err.contains("cannot run inside the mount"),
        "expected inside-mount rejection, got: {err}"
    );
}

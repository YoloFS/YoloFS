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

/// Auto-snapshot is skipped when the run command produces no changes.
#[test]
fn run_no_changes_skips_snapshot() {
    let session = YoloSession::new().expect("session setup");

    let before = session.cli(&["timeline"]).expect("timeline before");
    let before_count = before.matches("snapshot ").count();

    // Run a read-only command — no staged changes
    let code = session.run_in_yolofs(&["true"]).unwrap();
    assert_eq!(code, 0);

    let after = session.cli(&["timeline"]).expect("timeline after");
    let after_count = after.matches("snapshot ").count();

    assert_eq!(
        before_count, after_count,
        "no-op run should not create a snapshot.\nbefore:\n{before}\nafter:\n{after}"
    );
}

/// Auto-snapshot is created when the run command makes changes.
#[test]
fn run_with_changes_creates_snapshot() {
    let session = YoloSession::new().expect("session setup");

    let before = session.cli(&["timeline"]).expect("timeline before");
    let before_count = before.matches("snapshot ").count();

    let target = session.root.join("chk_test.txt");
    let code = session
        .run_in_yolofs(&["sh", "-c", &format!("echo hello > {}", target.display())])
        .unwrap();
    assert_eq!(code, 0);

    let after = session.cli(&["timeline"]).expect("timeline after");
    let after_count = after.matches("snapshot ").count();

    assert!(
        after_count == before_count + 1,
        "run with changes should create exactly one snapshot.\nbefore:\n{before}\nafter:\n{after}"
    );
}

// ── `yolo run -- <cmd>`: run, then review (vs quiet `yolo run -q --`) ────

/// `yolo run -- <cmd>` runs the command and then prints a status summary of what
/// it changed (the run-and-review path), unlike the quiet `yolo run -q --`.
#[test]
fn run_shorthand_shows_status() {
    let session = YoloSession::new().expect("session setup");

    let (ok, stdout, _err) = session
        .cli_output(&["run", "--", "sh", "-c", "echo hi > shorthand.txt"])
        .expect("yolo run -- cmd");

    assert!(ok, "command should succeed");
    assert!(
        stdout.contains("shorthand.txt"),
        "`yolo run -- <cmd>` should print a status summary naming the changed file: {stdout}"
    );
    assert!(
        stdout.contains("staged change"),
        "the summary should mention staged changes: {stdout}"
    );
}

/// `yolo run -- <cmd>` reviews what THAT command did. A no-op command run after a
/// real change must report no changes — not echo the previous command's staged
/// change (regression: it used to fall back to the last snapshot's batch).
#[test]
fn run_shorthand_noop_after_change_shows_no_changes() {
    let session = YoloSession::new().expect("session setup");

    // First command stages a change (auto-snapshots).
    session
        .cli_output(&["run", "--", "sh", "-c", "echo hi > made.txt"])
        .expect("first run");

    // Second command changes nothing — its review must say no changes, and must
    // NOT show the previous command's change.
    let (ok, stdout, _err) = session
        .cli_output(&["run", "--", "true"])
        .expect("no-op run");
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

/// `yolo run -q -- <cmd>` is the quiet form: it must NOT print a status summary
/// (the snapshot line it does emit goes to stderr).
#[test]
fn quiet_run_no_status() {
    let session = YoloSession::new().expect("session setup");

    let (ok, stdout, _err) = session
        .cli_output(&["run", "-q", "--", "sh", "-c", "echo hi > quiet.txt"])
        .expect("yolo run -q -- cmd");

    assert!(ok, "command should succeed");
    assert!(
        !stdout.contains("staged change"),
        "`yolo run -q --` should not print a status summary on stdout: {stdout}"
    );
}

/// The shorthand propagates the command's exit code.
#[test]
fn run_shorthand_propagates_exit_code() {
    let session = YoloSession::new().expect("session setup");

    let code = session
        .cli_exit_code(&["run", "--", "sh", "-c", "exit 42"])
        .unwrap();
    assert_eq!(
        code, 42,
        "`yolo run -- <cmd>` should propagate the command's exit code"
    );
}

/// `yolo run -- yolo <sub>` (the agent path) gates which yolo commands an agent
/// may run: read/navigation pass through; commit/abort/rule/etc. are the human's.
#[test]
fn agent_yolo_subcommands_are_gated() {
    let session = YoloSession::new().expect("session setup");

    // Allowed: `review` runs (fresh session ⇒ "No changes staged").
    let (ok, out, _err) = session
        .cli_output(&["run", "--", "yolo", "review"])
        .expect("yolo run -- yolo review");
    assert!(ok, "review should be allowed for the agent: {out}");

    // Blocked: commit/abort/rule are reserved for the human.
    for sub in ["commit", "abort", "rule"] {
        let (ok, _out, err) = session
            .cli_output(&["run", "--", "yolo", sub])
            .expect("blocked yolo subcommand");
        assert!(!ok, "`yolo {sub}` should be blocked for the agent: {err}");
        assert!(
            err.contains("reserved for the human"),
            "expected the reserved-for-human message for `{sub}`: {err}"
        );
    }
}

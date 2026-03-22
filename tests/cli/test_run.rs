use crate::helpers::AgfsSession;

#[test]
fn run_success_exit_code_zero() {
    let session = AgfsSession::new().expect("session setup");
    let code = session.run_in_sandbox(&["true"]).unwrap();
    assert_eq!(code, 0, "successful command should return exit code 0");
}

#[test]
fn run_failure_exit_code_propagated() {
    let session = AgfsSession::new().expect("session setup");
    let code = session.run_in_sandbox(&["false"]).unwrap();
    assert_eq!(code, 1, "`false` should return exit code 1");
}

#[test]
fn run_custom_exit_code() {
    let session = AgfsSession::new().expect("session setup");
    let code = session.run_in_sandbox(&["sh", "-c", "exit 42"]).unwrap();
    assert_eq!(code, 42, "exit 42 should propagate as exit code 42");
}

#[test]
fn run_command_not_found() {
    let session = AgfsSession::new().expect("session setup");
    let code = session.run_in_sandbox(&["nonexistent_cmd_xyz"]).unwrap();
    assert_ne!(
        code, 0,
        "nonexistent command should return non-zero exit code"
    );
}

#[test]
fn run_shell_pipe() {
    let session = AgfsSession::new().expect("session setup");
    let chroot_path = session.root.join("hello.txt");
    let code = session
        .run_in_sandbox(&[
            "sh",
            "-c",
            &format!("cat {} | grep base", chroot_path.display()),
        ])
        .unwrap();
    assert_eq!(code, 0, "shell pipe should work");
}

#[test]
fn run_shell_quotes() {
    let session = AgfsSession::new().expect("session setup");
    let chroot_path = session.root.join("hello.txt");
    let code = session
        .run_in_sandbox(&[
            "sh",
            "-c",
            &format!("test \"$(cat {})\" = 'base content'", chroot_path.display()),
        ])
        .unwrap();
    assert_eq!(code, 0, "shell quotes should be preserved");
}

#[test]
fn run_reads_file_through_mount() {
    let session = AgfsSession::new().expect("session setup");
    let chroot_path = session.root.join("hello.txt");
    let code = session
        .run_in_sandbox(&["cat", chroot_path.to_str().unwrap()])
        .unwrap();
    assert_eq!(
        code, 0,
        "cat should succeed reading a file inside the sandbox"
    );
}

/// Writing to a file via absolute path inside the sandbox should succeed.
#[test]
fn run_write_file_absolute_path() {
    let session = AgfsSession::new().expect("session setup");
    let target = session.root.join("exec_output.txt");
    let code = session
        .run_in_sandbox(&["sh", "-c", &format!("echo hello > {}", target.display())])
        .unwrap();
    assert_eq!(code, 0, "writing to absolute path should succeed");
}

/// Writing to a file via relative path inside the sandbox should succeed.
/// The cwd after pivot_root+chdir is the session root directory.
#[test]
fn run_write_file_relative_path() {
    let session = AgfsSession::new().expect("session setup");
    let code = session
        .run_in_sandbox(&["sh", "-c", "echo hello > relative_test.txt"])
        .unwrap();
    assert_eq!(
        code, 0,
        "writing to a relative path inside the sandbox should succeed"
    );
}

#[test]
fn run_env_var_propagated() {
    let session = AgfsSession::new().expect("session setup");
    // AGFS_SESSION is always set by exec.rs — verify the sandbox sees it
    let code = session
        .run_in_sandbox(&["sh", "-c", "test -n \"$AGFS_SESSION\""])
        .unwrap();
    assert_eq!(code, 0, "AGFS_SESSION env var should be set in sandbox");
}

#[test]
fn run_stderr_output() {
    let session = AgfsSession::new().expect("session setup");
    let code = session
        .run_in_sandbox(&["sh", "-c", "echo err >&2"])
        .unwrap();
    assert_eq!(code, 0, "writing to stderr should succeed");
}

/// Verify modified file is visible inside the sandbox.
#[test]
fn run_reads_modified_file() {
    let session = AgfsSession::new().expect("session setup");
    let target = session.root.join("hello.txt");
    // Write through agfs exec
    let code = session
        .run_in_sandbox(&["sh", "-c", &format!("echo modified > {}", target.display())])
        .unwrap();
    assert_eq!(code, 0, "write should succeed");
    // Read back through agfs exec — should see modified content
    let code = session
        .run_in_sandbox(&[
            "sh",
            "-c",
            &format!("grep -q modified {}", target.display()),
        ])
        .unwrap();
    assert_eq!(code, 0, "sandbox should see modified file content");
}

#[test]
fn run_multiple_commands_sequentially() {
    let session = AgfsSession::new().expect("session setup");
    let target = session.root.join("seq_test.txt");

    let code1 = session
        .run_in_sandbox(&["sh", "-c", &format!("echo first > {}", target.display())])
        .unwrap();
    assert_eq!(code1, 0);

    let code2 = session
        .run_in_sandbox(&["sh", "-c", &format!("grep -q first {}", target.display())])
        .unwrap();
    assert_eq!(code2, 0, "second command should see first command's output");
}

/// Auto-checkpoint is skipped when the exec command produces no changes.
#[test]
fn run_no_changes_skips_checkpoint() {
    let session = AgfsSession::new().expect("session setup");
    let before = session.cli(&["timeline"]).expect("timeline before");
    let before_count = before.matches("checkpoint").count();

    // Run a read-only command — no staged changes
    let code = session.run_in_sandbox(&["true"]).unwrap();
    assert_eq!(code, 0);

    let after = session.cli(&["timeline"]).expect("timeline after");
    let after_count = after.matches("checkpoint").count();

    assert_eq!(
        before_count, after_count,
        "no-op exec should not create a checkpoint.\nbefore:\n{before}\nafter:\n{after}"
    );
}

/// Auto-checkpoint is created when the exec command makes changes.
#[test]
fn run_with_changes_creates_checkpoint() {
    let session = AgfsSession::new().expect("session setup");
    let before = session.cli(&["timeline"]).expect("timeline before");
    let before_count = before.matches("checkpoint [").count();

    let target = session.root.join("chk_test.txt");
    let code = session
        .run_in_sandbox(&["sh", "-c", &format!("echo hello > {}", target.display())])
        .unwrap();
    assert_eq!(code, 0);

    let after = session.cli(&["timeline"]).expect("timeline after");
    let after_count = after.matches("checkpoint [").count();

    assert!(
        after_count == before_count + 1,
        "exec with changes should create exactly one checkpoint.\nbefore:\n{before}\nafter:\n{after}"
    );
}

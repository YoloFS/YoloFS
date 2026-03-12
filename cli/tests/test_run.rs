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

    let code = session.run_in_sandbox(&["exit 42"]).unwrap();
    assert_eq!(code, 42, "exit 42 should propagate as exit code 42");
}

#[test]
fn run_command_not_found() {
    let session = AgfsSession::new().expect("session setup");

    let code = session.run_in_sandbox(&["nonexistent_cmd_xyz"]).unwrap();
    assert_ne!(code, 0, "nonexistent command should return non-zero exit code");
}

#[test]
fn run_shell_pipe() {
    let session = AgfsSession::new().expect("session setup");

    let chroot_path = session.root.join("hello.txt");
    let code = session
        .run_in_sandbox(&[&format!("cat {} | grep base", chroot_path.display())])
        .unwrap();
    assert_eq!(code, 0, "shell pipe should work");
}

#[test]
fn run_shell_quotes() {
    let session = AgfsSession::new().expect("session setup");

    let chroot_path = session.root.join("hello.txt");
    let code = session
        .run_in_sandbox(&[&format!("test \"$(cat {})\" = 'base content'", chroot_path.display())])
        .unwrap();
    assert_eq!(code, 0, "shell quotes should be preserved");
}

#[test]
fn run_reads_file_through_mount() {
    let session = AgfsSession::new().expect("session setup");

    // Inside the chroot, paths are relative to the mount root (which IS /)
    // The test file lives at <root>/hello.txt, visible as <root>/hello.txt inside chroot
    let chroot_path = session.root.join("hello.txt");
    let code = session.run_in_sandbox(&["cat", chroot_path.to_str().unwrap()]).unwrap();
    assert_eq!(code, 0, "cat should succeed reading a file inside the sandbox");
}

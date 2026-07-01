use crate::helpers::YoloSession;
use std::process::{Command, Stdio};

#[test]
fn unmount_command_cleans_up() {
    let session = YoloSession::new().expect("session setup");
    let yolo_dir = session.root.join(".yolofs");

    assert!(yolo_dir.join("mnt").exists(), "mnt exists before unmount");

    let (ok, _, stderr) = session.cli_output(&["unmount"]).unwrap();
    assert!(ok, "unmount should succeed: {stderr}");
    assert!(
        yolo_dir.exists(),
        ".yolofs/ artifact should remain after unmount"
    );
}

#[test]
fn unmount_preserves_and_mount_restores_staging() {
    let session = YoloSession::new().expect("session setup");
    let yolo_dir = session.root.join(".yolofs");
    std::fs::write(session.mnt_path("hello.txt"), "unmounted\n").unwrap();

    let stderr = session.cli_stderr(&["unmount"]).unwrap();
    assert!(stderr.contains("unmounted"), "stderr: {stderr}");
    assert!(yolo_dir.exists(), "unmounted artifact should remain");
    assert!(!session.mnt.exists(), "live view should be gone");

    let stderr = session.cli_stderr(&["mount"]).unwrap();
    assert!(
        stderr.contains("restored staged changes"),
        "stderr: {stderr}"
    );
    assert_eq!(
        std::fs::read_to_string(session.mnt_path("hello.txt")).unwrap(),
        "unmounted\n"
    );
}

#[test]
fn commit_and_abort_work_without_live_view() {
    let session = YoloSession::new().expect("session setup");
    let yolo_dir = session.root.join(".yolofs");

    std::fs::write(session.mnt_path("hello.txt"), "commit offline\n").unwrap();
    session.cli(&["unmount"]).unwrap();
    session.cli(&["commit"]).unwrap();
    assert_eq!(
        std::fs::read_to_string(session.base_path("hello.txt")).unwrap(),
        "commit offline\n"
    );
    assert!(yolo_dir.exists(), "commit must not remove the artifact");

    session.cli(&["mount"]).unwrap();
    std::fs::write(session.mnt_path("hello.txt"), "abort offline\n").unwrap();
    session.cli(&["unmount"]).unwrap();
    session.cli(&["abort", "--force"]).unwrap();
    assert_eq!(
        std::fs::read_to_string(session.base_path("hello.txt")).unwrap(),
        "commit offline\n"
    );
    assert!(yolo_dir.exists(), "abort must not remove the artifact");
}

#[test]
fn double_mount_is_idempotent() {
    let session = YoloSession::new().expect("session setup");

    let (ok, _, stderr) = session.cli_output(&["mount"]).unwrap();
    assert!(ok, "second mount should succeed (idempotent): {stderr}");
    assert!(
        stderr.contains("already mounted at"),
        "second mount should say it's a no-op: {stderr}"
    );
}

#[test]
fn cwd_symlink_created() {
    let session = YoloSession::new().expect("session setup");
    let cwd_link = session.root.join(".yolofs/cwd");

    assert!(
        cwd_link
            .symlink_metadata()
            .unwrap()
            .file_type()
            .is_symlink(),
        ".yolofs/cwd should be a symlink"
    );

    let target = std::fs::read_link(&cwd_link).unwrap();
    let expected_suffix = session.root.strip_prefix("/").unwrap();
    assert!(
        target.ends_with(expected_suffix),
        "symlink target {target:?} should end with {expected_suffix:?}"
    );
}

/// `/proc`, `/sys`, `/dev` are mounted per-command inside each `yolo run`'s
/// private namespace (a fresh procfs plus `/dev` `/sys` binds), so they are
/// usable from inside a command.
#[test]
fn pseudofs_available_inside_run() {
    let session = YoloSession::new().expect("session setup");
    // Check /proc/self exists, /sys is a dir, and /dev is a *functional*
    // devtmpfs — /dev/null is a character device and is writable — not just a
    // present path (a wrong/empty bind would still pass `test -e`).
    let code = session
        .run_in_yolofs(&[
            "sh",
            "-c",
            "test -e /proc/self && test -d /sys && test -c /dev/null && echo x > /dev/null",
        ])
        .expect("run pseudofs check");
    assert_eq!(
        code, 0,
        "/proc, /sys and a functional /dev should be available inside a `yolo run` command"
    );
}

/// The command runs as PID 1 of its own pid namespace, backed by a fresh
/// `/proc` — so `$$` reports 1, confirming the pid namespace is active.
#[test]
fn command_runs_in_private_pid_namespace() {
    let session = YoloSession::new().expect("session setup");
    let (ok, out, err) = session
        .cli_output(&["run", "--no-review", "--", "sh", "-c", "echo pid=$$"])
        .expect("run pid check");
    assert!(ok, "run should succeed: {err}");
    assert!(
        out.contains("pid=1"),
        "command should be PID 1 in its own pid namespace: {out:?}"
    );
}

/// `yolo mount` no longer creates persistent `/proc` `/sys` `/dev` mounts in the
/// host mount namespace — they live per-command in each `yolo run`'s private
/// namespace and are reaped when it exits. So while mounted, the host mount
/// table has no pseudo-fs under the yolofs runtime mountpoint (and nothing to
/// unbind at unmount).
#[test]
fn mount_does_not_persist_pseudofs() {
    let session = YoloSession::new().expect("session setup");

    let mountinfo = std::fs::read_to_string("/proc/self/mountinfo").expect("read mountinfo");
    let leaked: Vec<&str> = mountinfo
        .lines()
        .filter(|line| {
            // mountpoint is field index 4; fstype follows the " - " separator.
            let under_yolofs = line
                .split_whitespace()
                .nth(4)
                .is_some_and(|mp| mp.contains("/yolofs/"));
            let is_pseudofs = line.split(" - ").nth(1).is_some_and(|rest| {
                // Trailing space anchors the fstype token so it can't prefix-match
                // a longer name (e.g. "proc" vs a hypothetical "procfoo").
                ["proc ", "sysfs ", "devtmpfs ", "devpts "]
                    .iter()
                    .any(|t| rest.starts_with(t))
            });
            under_yolofs && is_pseudofs
        })
        .collect();
    assert!(
        leaked.is_empty(),
        "no pseudo-fs should persist under the yolofs mount: {leaked:?}"
    );

    let (ok, _, stderr) = session.cli_output(&["unmount"]).unwrap();
    assert!(ok, "unmount should succeed: {stderr}");
    assert!(
        session.root.join(".yolofs").exists(),
        ".yolofs/ artifact should remain"
    );
}

/// When a process holds an fd on the mount, unmount should fail with a
/// message identifying the blocking process (stdin is /dev/null in tests,
/// so the interactive kill-prompt is auto-declined).
#[test]
fn unmount_reports_blocking_process() {
    let session = YoloSession::new().expect("session setup");

    // Spawn a child that holds a file on the yolofs mount open.
    let file_in_mount = session.mnt_path("hello.txt");
    let mut child = Command::new("bash")
        .args([
            "-c",
            &format!("exec 3<'{}'; sleep 60", file_in_mount.display()),
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn blocker");
    let child_pid = child.id();

    // Give the child time to open the fd.
    std::thread::sleep(std::time::Duration::from_millis(300));

    // Unmount should fail — stdin is /dev/null so the kill-prompt is declined.
    let (ok, _, stderr) = session.cli_output(&["unmount"]).unwrap();
    assert!(!ok, "unmount should fail when mount is busy");
    assert!(
        stderr.contains("busy"),
        "error should mention 'busy': {stderr}"
    );
    assert!(
        stderr.contains(&child_pid.to_string()),
        "error should list blocking PID {child_pid}: {stderr}"
    );

    // Kill the blocker, then unmount should succeed.
    let _ = Command::new("kill").arg(child_pid.to_string()).status();
    let _ = child.wait();
    std::thread::sleep(std::time::Duration::from_millis(300));

    let (ok, _, stderr) = session.cli_output(&["unmount"]).unwrap();
    assert!(ok, "unmount should succeed after blocker killed: {stderr}");
}

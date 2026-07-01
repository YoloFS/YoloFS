use crate::helpers::{YOLO_BIN, YoloSession};
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

#[test]
fn pseudofs_bind_mounted() {
    let session = YoloSession::new().expect("session setup");

    // /proc, /sys, /dev should be visible inside the mount
    for name in &["proc", "sys", "dev"] {
        let path = session.mnt.join(name);
        assert!(path.exists(), "{name} should exist in mount");
        assert!(path.is_dir(), "{name} should be a directory");
    }

    // /proc/self should be accessible (confirms it's a real procfs, not empty dir)
    let proc_self = session.mnt.join("proc/self");
    assert!(
        proc_self.exists(),
        "/proc/self should be accessible via bind-mount"
    );
}

#[test]
fn unmount_cleans_up_pseudofs() {
    let session = YoloSession::new().expect("session setup");
    let mnt = session.root.join(".yolofs/mnt");

    // Verify bind-mounts are present
    assert!(
        mnt.join("proc/self").exists(),
        "proc should be bind-mounted"
    );

    let (ok, _, stderr) = session.cli_output(&["unmount"]).unwrap();
    assert!(ok, "unmount should succeed with bind-mounts: {stderr}");
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

/// `yolo unload` right after SIGKILLing a process with files open in the
/// mount must not fail with EAGAIN: the kernel can release the module's last
/// reference asynchronously after `umount(2)` returns (deferred fput /
/// superblock teardown), and unload briefly retries `delete_module(2)` to
/// cover that window. The kill→unload cycle loops to widen the race window,
/// but the race did not reproduce in ~90 cycles on the test VM — this is a
/// regression smoke test, not a reliable reproducer (see
/// docs/plans/55-unload-retry.md).
///
/// Manual setup instead of `YoloSession`: unloading the module writes
/// "yolofs: module unloaded" to the kernel log, which the session's
/// kmsg-cleanliness check on drop would flag as a failure.
#[test]
fn unload_retries_until_module_quiesces() {
    for cycle in 0..10 {
        let root = crate::helpers::session_tempdir().expect("temp dir").keep();
        std::fs::write(root.join("hello.txt"), "base content\n").unwrap();
        yolofs::config::Config {
            permission: false,
            ..Default::default()
        }
        .save(&root.join("yolofs.toml"))
        .unwrap();

        let out = Command::new(YOLO_BIN)
            .arg("mount")
            .current_dir(&root)
            .env("NO_COLOR", "1")
            .output()
            .expect("run yolo mount");
        assert!(
            out.status.success(),
            "mount (cycle {cycle}): {}",
            String::from_utf8_lossy(&out.stderr)
        );

        // Child holding an open fd on a file inside the mount.
        let file_in_mount = root
            .join(".yolofs/mnt")
            .join(root.strip_prefix("/").unwrap())
            .join("hello.txt");
        let mut child = Command::new("bash")
            .args([
                "-c",
                &format!("exec 3<'{}'; sleep 60", file_in_mount.display()),
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn blocker");
        std::thread::sleep(std::time::Duration::from_millis(200));

        // SIGKILL and reap, then unload immediately: once wait() returns the
        // child's fds are closed, but the module refcount can drop later.
        child.kill().expect("kill blocker");
        child.wait().expect("reap blocker");

        let out = Command::new(YOLO_BIN)
            .arg("unload")
            .current_dir(&root)
            .env("NO_COLOR", "1")
            .output()
            .expect("run yolo unload");
        assert!(
            out.status.success(),
            "unload (cycle {cycle}): {}",
            String::from_utf8_lossy(&out.stderr)
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    // Leave the module loaded: the next test's session snapshots the kernel
    // log before mounting, and an auto-load during its mount would make its
    // kmsg-cleanliness check trip on the "module loaded" message.
    let out = Command::new(YOLO_BIN)
        .arg("load")
        .output()
        .expect("run yolo load");
    assert!(out.status.success(), "reload module after test");
}

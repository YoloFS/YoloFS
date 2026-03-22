use crate::helpers::AgfsSession;
use std::process::{Command, Stdio};

#[test]
fn unmount_command_cleans_up() {
    let session = AgfsSession::new().expect("session setup");
    let agfs_dir = session.root.join(".agfs");

    assert!(agfs_dir.join("mnt").exists(), "mnt exists before unmount");

    // Unmount must be called from the host (outside the namespace),
    // since it signals the daemon to shut down.
    let (ok, _, stderr) = session.cli_output(&["unmount"]).unwrap();
    assert!(ok, "unmount should succeed: {stderr}");
    assert!(!agfs_dir.exists(), ".agfs/ should be removed after unmount");
}

#[test]
fn double_mount_is_idempotent() {
    let session = AgfsSession::new().expect("session setup");
    session.run_in_namespace(|| {
        let (ok, _, stderr) = session.cli_output(&["mount"]).unwrap();
        assert!(ok, "second mount should succeed (idempotent): {stderr}");
    });
}

#[test]
fn cwd_symlink_created() {
    let session = AgfsSession::new().expect("session setup");
    session.run_in_namespace(|| {
        let cwd_link = session.root.join(".agfs/cwd");

        assert!(
            cwd_link
                .symlink_metadata()
                .unwrap()
                .file_type()
                .is_symlink(),
            ".agfs/cwd should be a symlink"
        );

        let target = std::fs::read_link(&cwd_link).unwrap();
        let expected_suffix = session.root.strip_prefix("/").unwrap();
        assert!(
            target.ends_with(expected_suffix),
            "symlink target {target:?} should end with {expected_suffix:?}"
        );
    });
}

#[test]
fn pseudofs_mounted() {
    let session = AgfsSession::new().expect("session setup");
    session.run_in_namespace(|| {
        // /proc and /dev should be visible inside the mount
        for name in &["proc", "dev"] {
            let path = session.mnt.join(name);
            assert!(path.exists(), "{name} should exist in mount");
            assert!(path.is_dir(), "{name} should be a directory");
        }

        // /proc/1 should be accessible (the daemon is PID 1 in the PID namespace;
        // confirms a real procfs is mounted, not just an empty dir)
        let proc_1 = session.mnt.join("proc/1");
        assert!(
            proc_1.exists(),
            "/proc/1 should be accessible (daemon is PID 1 in PID namespace)"
        );

        // /dev/null should be accessible (confirms device nodes are set up)
        let dev_null = session.mnt.join("dev/null");
        assert!(dev_null.exists(), "/dev/null should be accessible");
    });
}

#[test]
fn unmount_cleans_up_pseudofs() {
    let session = AgfsSession::new().expect("session setup");

    // Verify proc is mounted (from inside namespace)
    session.run_in_namespace(|| {
        let mnt = session.root.join(".agfs/mnt");
        assert!(mnt.join("proc/1").exists(), "proc should be mounted");
    });

    // Unmount from host (signals daemon to shut down)
    let (ok, _, stderr) = session.cli_output(&["unmount"]).unwrap();
    assert!(ok, "unmount should succeed: {stderr}");
    assert!(
        !session.root.join(".agfs").exists(),
        ".agfs/ should be removed"
    );
}

/// With the namespace architecture, unmount kills the daemon and the kernel
/// cleans up the namespace. There is no "busy mount" scenario from the host.
/// This test verified the old behavior where umount() would fail with EBUSY.
/// Kept as a placeholder for future: unmount should handle active exec sessions.
#[test]
#[ignore = "busy-mount detection moved to namespace daemon lifecycle"]
fn unmount_reports_blocking_process() {
    let session = AgfsSession::new().expect("session setup");

    // Spawn a blocker inside the namespace via agfs exec. It holds a file
    // open and sleeps. We run it in the background so we can attempt
    // unmount from the host while it's still alive.
    let file_path = session.root.join("hello.txt");
    let mut blocker = Command::new(crate::helpers::AGFS_BIN)
        .args([
            "exec",
            "--",
            "bash",
            "-c",
            &format!("exec 3<'{}'; sleep 60", file_path.display()),
        ])
        .current_dir(&session.root)
        .env("NO_COLOR", "1")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn blocker via agfs exec");
    let blocker_pid = blocker.id();

    // Give it time to enter the namespace and open the fd.
    std::thread::sleep(std::time::Duration::from_millis(300));

    // Unmount from host should fail — the blocker holds an fd.
    let (ok, _, stderr) = session.cli_output(&["unmount"]).unwrap();
    assert!(!ok, "unmount should fail when mount is busy: {stderr}");
    assert!(
        stderr.contains("busy"),
        "error should mention 'busy': {stderr}"
    );

    // Kill the blocker, then unmount should succeed.
    let _ = nix::sys::signal::kill(
        nix::unistd::Pid::from_raw(blocker_pid as i32),
        nix::sys::signal::Signal::SIGTERM,
    );
    let _ = blocker.wait();
    std::thread::sleep(std::time::Duration::from_millis(200));

    let (ok, _, stderr) = session.cli_output(&["unmount"]).unwrap();
    assert!(ok, "unmount should succeed after blocker killed: {stderr}");
}

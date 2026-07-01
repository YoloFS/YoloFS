use crate::helpers::YoloSession;
use std::path::Path;
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

/// Regression: deleting the project directory *before* unmounting used to
/// orphan the kernel mount. `unmount_at`/`unmount_all` resolved the live
/// mountpoint through the `.yolofs/mnt` symlink, which the delete removed — so
/// the umount was skipped, the module reference never dropped, and `yolo
/// unload` failed with "module still has 1 reference(s)". `unload` now unmounts
/// the mountpoint the kernel reports in `/proc/mounts`, independent of any
/// in-workspace symlink, and sweeps the stale mountpoint dir left behind.
///
/// This drives the *global* kernel module (unload then load), so it manages
/// that state directly rather than through `YoloSession`, and restores the
/// module to loaded before asserting — the serial e2e runner shares one loaded
/// module across tests, and a stray "module loaded" line would trip the next
/// test's kmsg check.
#[test]
fn unload_after_project_dir_deleted() {
    use yolofs::config::Config;

    // A throwaway project on the root fs (yolofs can't see submounts).
    let dir = crate::helpers::session_tempdir().unwrap().keep();
    Config {
        permission: false,
        ..Default::default()
    }
    .save(&dir.join("yolofs.toml"))
    .unwrap();
    std::fs::write(dir.join("file.txt"), "hi\n").unwrap();

    let out = Command::new("yolo")
        .arg("mount")
        .current_dir(&dir)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "mount: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let source = dir.join(".yolofs");
    let mountpoint = yolofs_mountpoint(&source).expect("mount should be listed in /proc/mounts");

    // The user's mistake: `rm -rf project/` before unmounting — takes the
    // `.yolofs/mnt` symlink with it, orphaning the kernel mount.
    std::fs::remove_dir_all(&dir).unwrap();

    let unload = Command::new("yolo")
        .arg("unload")
        .current_dir("/")
        .output()
        .unwrap();
    let still_mounted = yolofs_mountpoint(&source).is_some();
    let mountpoint_dir_left = Path::new(&mountpoint).exists();

    // Restore the module for the rest of the serial suite BEFORE any assert can
    // unwind. A failed unload leaves it loaded; a successful one leaves it
    // unloaded, so reload it (a no-op if already loaded).
    let _ = Command::new("yolo").arg("load").current_dir("/").output();

    let stderr = String::from_utf8_lossy(&unload.stderr);
    assert!(
        unload.status.success(),
        "unload should succeed after project dir deleted: {stderr}"
    );
    assert!(
        !still_mounted,
        "orphaned mount should be gone from /proc/mounts"
    );
    assert!(
        !mountpoint_dir_left,
        "stale runtime mountpoint dir should be swept: {mountpoint}"
    );
}

/// The mountpoint the kernel records for a YoloFS `source` (its `.yolofs` dir),
/// read from `/proc/mounts`. `None` if it is not mounted.
fn yolofs_mountpoint(source: &Path) -> Option<String> {
    let src = source.to_string_lossy();
    let content = std::fs::read_to_string("/proc/mounts").ok()?;
    content.lines().find_map(|line| {
        let mut cols = line.split_whitespace();
        let s = cols.next()?;
        let mp = cols.next()?;
        let fstype = cols.next()?;
        (fstype == "yolofs" && s == src).then(|| mp.to_string())
    })
}

use crate::helpers::YoloSession;
use std::fs;
use std::process::{Command, Stdio};
use yolofs::config::Config;

/// A command run through `yolo run` must not be able to reach the host
/// filesystem behind the mount via the `/proc/<pid>/root` magic symlink of an
/// ordinary same-uid process living *outside* the mount.
///
/// Under a plain `chroot` the command shares the host pid + mount namespaces, so
/// such a symlink resolves straight to the real root and bypasses gating. With a
/// private pid namespace and a fresh `/proc`, the outside pid is not even
/// visible, so the read fails.
#[test]
fn proc_root_symlink_cannot_escape_the_mount() {
    // Default config: permission=true with system dirs read-only (so the command
    // and its interpreter run) and the session root allowed. Anything else — the
    // secret below included — is the default `ask`, denied with no watcher.
    let s = YoloSession::new_with_config(Config::default()).expect("session setup");

    // A secret on the host, OUTSIDE the session root (a sibling dir under the
    // shared tests root), so the permission layer denies it through the mount.
    let secret_path = s
        .root
        .parent()
        .expect("session root has a parent")
        .join("escape-secret.txt");
    fs::write(&secret_path, "TOP-SECRET-HOST-DATA\n").expect("write secret");
    let secret_abs = secret_path.to_string_lossy().to_string();

    // Sanity: reading it straight through the mount is denied.
    let (ok_direct, _, _) = s
        .cli_output(&["run", "--no-review", "--", "cat", &secret_abs])
        .expect("run cat direct");
    assert!(
        !ok_direct,
        "precondition: direct read of the out-of-session secret should be denied"
    );

    // An ordinary same-uid process living OUTSIDE the mount (root = real /).
    let mut outside = Command::new("sleep")
        .arg("300")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn outside process");
    let host_pid = outside.id();

    // Attempt the escape: read the secret through that process's root symlink.
    let proc_path = format!("/proc/{host_pid}/root{secret_abs}");
    let (ok_escape, out, err) = s
        .cli_output(&["run", "--no-review", "--", "cat", &proc_path])
        .expect("run cat via /proc root");

    // The mechanism, directly: the outside pid must not even exist in the
    // command's fresh /proc (this pins the failure to pid-namespace isolation,
    // not to permission gating — /proc is a private procfs, not gated).
    let pid_visible = s
        .run_in_yolofs(&["test", "-e", &format!("/proc/{host_pid}")])
        .expect("run pid-visibility check")
        == 0;

    let leaked = out.contains("TOP-SECRET-HOST-DATA");
    outside.kill().ok();
    let _ = outside.wait();
    let _ = fs::remove_file(&secret_path);

    assert!(
        !pid_visible,
        "outside pid {host_pid} should be invisible in the command's fresh /proc"
    );
    assert!(
        !ok_escape && !leaked,
        "/proc/<pid>/root escaped the mount: ok={ok_escape} stdout={out:?} stderr={err:?}"
    );
}

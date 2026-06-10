use crate::helpers::YoloSession;

fn assert_raw_ioctl_rejected_from_inside(session: &YoloSession, request: u64, payload: &str) {
    let script = format!(
        r#"
import errno, fcntl, os, struct, sys
fd = os.open("/", os.O_RDONLY)
try:
    fcntl.ioctl(fd, {request}, {payload})
except OSError as e:
    sys.exit(0 if e.errno == errno.EPERM else 2)
else:
    sys.exit(1)
"#
    );
    let (ok, _, stderr) = session
        .cli_output(&["run", "--no-review", "--", "python3", "-c", &script])
        .expect("run python raw ioctl");
    assert!(ok, "inside ioctl should be rejected with EPERM: {stderr}");
}

#[test]
fn restore_rejected_from_inside_mount() {
    let session = YoloSession::new().expect("session setup");
    let restore_iow = (1u64 << 30) | (32u64 << 16) | (u64::from(b'A') << 8) | 42;
    assert_raw_ioctl_rejected_from_inside(
        &session,
        restore_iow,
        "struct.pack('QQQIB3x', 0, 0, 0, 0, 0)",
    );
}

#[test]
fn travel_rejected_from_inside_mount() {
    let session = YoloSession::new().expect("session setup");
    let travel_iowr = (3u64 << 30) | (32u64 << 16) | (u64::from(b'A') << 8) | 41;
    assert_raw_ioctl_rejected_from_inside(&session, travel_iowr, "struct.pack('QQQQ', 0, 0, 0, 0)");
}

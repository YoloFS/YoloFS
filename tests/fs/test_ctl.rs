//! Control moved from a synthetic `.ctl` file to the mount-root directory, so
//! `.ctl` should no longer exist. The ioctl surface itself is exercised by the
//! snapshot / rule / ask e2e tests.
use crate::helpers::YoloSession;

/// The synthetic `.ctl` control file is gone — control ioctls are now issued on
/// the mount-root directory instead.
#[test]
fn no_ctl_file_at_mount_root() {
    let s = YoloSession::new().expect("session setup");
    assert!(
        !s.mnt.join(".ctl").exists(),
        ".ctl should no longer exist; control is on the mount-root directory"
    );
}

//! White-box checks of RULE_SET's fd-based target validation: the kernel must
//! reject target fds that don't name an attachable dentry on this mount.

use crate::helpers::YoloSession;
use std::fs;
use std::os::unix::io::AsRawFd;
use yolofs::ioctl;

fn ctl(s: &YoloSession) -> fs::File {
    ioctl::open(&s.root.join(".yolofs")).expect("open mount-root ctl fd")
}

/// An fd on the base filesystem (not opened through the mount) names the same
/// tree but the wrong superblock — attaching a rule to it must fail.
#[test]
fn rule_set_rejects_fd_outside_the_mount() {
    let s = YoloSession::new().expect("session setup");
    let outside = ioctl::open_rule_target(s.root.join("hello.txt")).expect("open base file");
    let err = ioctl::set_rule_raw(&ctl(&s), outside.as_raw_fd(), ioctl::YOLO_PERM_DENY)
        .expect_err("rule on a base-fs fd must be rejected");
    assert_eq!(err, nix::errno::Errno::EXDEV);
}

/// An fd can outlive its name — something the old path-string contract could
/// never produce. A rule there would pin a dentry no lookup reaches.
#[test]
fn rule_set_rejects_unlinked_target() {
    let s = YoloSession::new().expect("session setup");
    fs::write(s.mnt_path("victim.txt"), "doomed\n").expect("create through mount");
    let target =
        ioctl::open_rule_target(s.mnt_path("victim.txt")).expect("open target through mount");
    fs::remove_file(s.mnt_path("victim.txt")).expect("unlink through mount");
    let err = ioctl::set_rule_raw(&ctl(&s), target.as_raw_fd(), ioctl::YOLO_PERM_DENY)
        .expect_err("rule on an unlinked target must be rejected");
    assert_eq!(err, nix::errno::Errno::EINVAL);
}

/// A closed or never-opened target fd fails with EBADF.
#[test]
fn rule_set_rejects_bad_fd() {
    let s = YoloSession::new().expect("session setup");
    let err = ioctl::set_rule_raw(&ctl(&s), -1, ioctl::YOLO_PERM_DENY)
        .expect_err("rule on a bogus fd must be rejected");
    assert_eq!(err, nix::errno::Errno::EBADF);
}

/// The kernel takes any fd whose dentry is on the mount, not just O_PATH ones
/// (fget_raw accepts both) — a plain read fd works as a rule target too.
#[test]
fn rule_set_accepts_regular_fd() {
    let s = YoloSession::new_with_config(yolofs::config::Config {
        rules: std::collections::BTreeMap::from([("/".into(), yolofs::perm::Perm::Allow)]),
        ..Default::default()
    })
    .expect("session setup");
    let target = fs::File::open(s.mnt_path("hello.txt")).expect("open regular read fd");
    ioctl::set_rule_raw(&ctl(&s), target.as_raw_fd(), ioctl::YOLO_PERM_DENY)
        .expect("regular fd should be a valid rule target");
    assert!(
        fs::read_to_string(s.mnt_path("hello.txt")).is_err(),
        "deny rule set via a regular fd must be enforced"
    );
}

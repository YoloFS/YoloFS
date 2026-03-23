use crate::helpers::AgfsSession;
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::os::unix::fs::PermissionsExt;

/// The .ctl control file should exist at the mount root.
#[test]
fn ctl_exists_at_mount_root() {
    let s = AgfsSession::new().expect("session setup");
    let ctl = s.mnt.join(".ctl");

    let meta = fs::metadata(&ctl).expect(".ctl should exist");
    assert!(meta.is_file(), ".ctl should be a regular file");
}

/// The .ctl control file should have mode 0600.
#[test]
fn ctl_has_correct_permissions() {
    let s = AgfsSession::new().expect("session setup");
    let ctl = s.mnt.join(".ctl");

    let meta = fs::metadata(&ctl).expect(".ctl should exist");
    let mode = meta.permissions().mode() & 0o7777;
    assert_eq!(mode, 0o600, "expected 0600, got {mode:#o}");
}

/// The .ctl control file should be openable for reading.
#[test]
fn ctl_can_be_opened() {
    let s = AgfsSession::new().expect("session setup");
    let ctl = s.mnt.join(".ctl");

    let file = fs::File::open(&ctl);
    assert!(file.is_ok(), "should be able to open .ctl: {:?}", file.err());
}

/// The .ctl control file should have zero size.
#[test]
fn ctl_has_zero_size() {
    let s = AgfsSession::new().expect("session setup");
    let ctl = s.mnt.join(".ctl");

    let meta = fs::metadata(&ctl).expect(".ctl should exist");
    assert_eq!(meta.size(), 0, ".ctl should have zero size");
}

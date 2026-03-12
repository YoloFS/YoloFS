use crate::helpers::AgfsSession;
use std::process::Command;

#[test]
fn unmount_command_cleans_up() {
    let session = AgfsSession::new().expect("session setup");
    let agfs_dir = session.root.join(".agfs");

    assert!(agfs_dir.join("mnt").exists(), "mnt exists before unmount");

    let (ok, _, stderr) = session.cli_output(&["unmount"]).unwrap();
    assert!(ok, "unmount should succeed: {stderr}");
    assert!(!agfs_dir.exists(), ".agfs/ should be removed after unmount");
}

#[test]
fn unmount_succeeds_when_pseudo_fs_already_unmounted() {
    let session = AgfsSession::new().expect("session setup");
    let agfs_dir = session.root.join(".agfs");
    let mnt = agfs_dir.join("mnt");

    // Manually unmount sys before calling agfs unmount to simulate the
    // case where a pseudo-fs is not mounted (the EINVAL bug).
    let sys_target = mnt.join("sys");
    if sys_target.exists() {
        let _ = Command::new("umount").arg(&sys_target).output();
    }

    let (ok, _, stderr) = session.cli_output(&["unmount"]).unwrap();
    assert!(ok, "unmount should succeed even when sys was already unmounted: {stderr}");
    assert!(!agfs_dir.exists(), ".agfs/ should be removed after unmount");
}

#[test]
fn double_mount_prevented() {
    let session = AgfsSession::new().expect("session setup");

    let (ok, _, stderr) = session.cli_output(&["mount"]).unwrap();
    assert!(!ok, "second mount should fail");
    assert!(
        stderr.contains("already mounted"),
        "should say already mounted, got: {stderr}"
    );
}

#[test]
fn cwd_symlink_created() {
    let session = AgfsSession::new().expect("session setup");
    let cwd_link = session.root.join(".agfs/cwd");

    assert!(
        cwd_link.symlink_metadata().unwrap().file_type().is_symlink(),
        ".agfs/cwd should be a symlink"
    );

    let target = std::fs::read_link(&cwd_link).unwrap();
    let expected_suffix = session.root.strip_prefix("/").unwrap();
    assert!(
        target.ends_with(expected_suffix),
        "symlink target {target:?} should end with {expected_suffix:?}"
    );
}

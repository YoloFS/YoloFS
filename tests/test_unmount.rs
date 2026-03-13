use crate::helpers::AgfsSession;

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
fn double_mount_is_idempotent() {
    let session = AgfsSession::new().expect("session setup");

    let (ok, _, stderr) = session.cli_output(&["mount"]).unwrap();
    assert!(ok, "second mount should succeed (idempotent): {stderr}");
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

#[test]
fn pseudofs_bind_mounted() {
    let session = AgfsSession::new().expect("session setup");

    // /proc, /sys, /dev should be visible inside the mount
    for name in &["proc", "sys", "dev"] {
        let path = session.mnt.join(name);
        assert!(path.exists(), "{name} should exist in mount");
        assert!(path.is_dir(), "{name} should be a directory");
    }

    // /proc/self should be accessible (confirms it's a real procfs, not empty dir)
    let proc_self = session.mnt.join("proc/self");
    assert!(proc_self.exists(), "/proc/self should be accessible via bind-mount");
}

#[test]
fn unmount_cleans_up_pseudofs() {
    let session = AgfsSession::new().expect("session setup");
    let mnt = session.root.join(".agfs/mnt");

    // Verify bind-mounts are present
    assert!(mnt.join("proc/self").exists(), "proc should be bind-mounted");

    let (ok, _, stderr) = session.cli_output(&["unmount"]).unwrap();
    assert!(ok, "unmount should succeed with bind-mounts: {stderr}");
    assert!(!session.root.join(".agfs").exists(), ".agfs/ should be removed");
}

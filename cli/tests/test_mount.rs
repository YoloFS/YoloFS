use crate::helpers::AgfsSession;
use crate::skip_if_not_root;

#[test]
fn mount_and_unmount() {
    skip_if_not_root!();
    let session = AgfsSession::new().expect("session setup");

    // Verify mount point exists and is accessible
    assert!(session.mnt.exists(), "mount point exists");
    assert!(session.mnt.join("tmp").exists(), "root fs visible through mount");

    // Verify test files visible through mount
    assert!(session.mnt_path("hello.txt").exists());
    assert!(session.mnt_path("subdir/deep.txt").exists());

    drop(session); // triggers unmount
}

#[test]
fn mount_creates_layout() {
    skip_if_not_root!();
    let session = AgfsSession::new().expect("session setup");

    assert!(session.agfs_dir.exists());
    assert!(session.staging.exists());
    assert!(session.mnt.exists());

    drop(session);
}

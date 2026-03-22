use crate::helpers::AgfsSession;
use nix::fcntl::{FallocateFlags, fallocate};
use std::fs::{self, OpenOptions};
use std::os::unix::fs::MetadataExt;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::io::AsRawFd;

#[test]
fn fallocate_passes_through_and_allocates_space() {
    let s = AgfsSession::new().expect("session setup");

    let path = s.mnt_path("fallocate.bin");
    let file = OpenOptions::new()
        .create(true)
        .write(true)
        .mode(0o644)
        .open(&path)
        .expect("create target file");

    fallocate(
        file.as_raw_fd(),
        FallocateFlags::empty(),
        0,
        16 * 1024 * 1024,
    )
    .expect("fallocate through agfs mount");
    drop(file);

    let st = fs::metadata(&path).expect("stat allocated file");
    assert_eq!(st.size(), 16 * 1024 * 1024);
    assert!(st.blocks() > 0, "fallocate should allocate blocks");

    let status = s.cli(&["status"]).expect("status");
    assert!(
        status.contains("fallocate.bin"),
        "status should include fallocate-created file: {status}"
    );
}

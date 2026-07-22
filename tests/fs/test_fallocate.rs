use crate::helpers::YoloSession;
use nix::errno::Errno;
use nix::fcntl::{FallocateFlags, fallocate};
use std::fs::{self, OpenOptions};
use std::os::unix::fs::MetadataExt;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::io::AsRawFd;
use std::path::Path;

/// Probe whether the backing filesystem supports `fallocate` at all.
///
/// yolofs is a pass-through: it forwards `fallocate` straight to the lower
/// file, so it can only allocate space when the underlying fs can. Some
/// filesystems can't — e.g. ext3 (indirect-mapped inodes, no unwritten-extent
/// support), which `ext4_fallocate` rejects with `EOPNOTSUPP`. On such a
/// backing store the allocation assertions below are meaningless, so we detect
/// the case and only verify that yolofs relays the error faithfully.
fn backing_fs_supports_fallocate(dir: &Path) -> bool {
    let probe = dir.join(".fallocate_probe");
    let file = OpenOptions::new()
        .create(true)
        .write(true)
        .mode(0o644)
        .open(&probe)
        .expect("create fallocate probe file");
    let res = fallocate(file.as_raw_fd(), FallocateFlags::empty(), 0, 4096);
    drop(file);
    let _ = fs::remove_file(&probe);
    !matches!(res, Err(Errno::EOPNOTSUPP))
}

#[test]
fn fallocate_passes_through_and_allocates_space() {
    let s = YoloSession::new().expect("session setup");

    let backing_supported = backing_fs_supports_fallocate(&s.root);

    let path = s.mnt_path("fallocate.bin");
    let file = OpenOptions::new()
        .create(true)
        .write(true)
        .mode(0o644)
        .open(&path)
        .expect("create target file");

    let res = fallocate(
        file.as_raw_fd(),
        FallocateFlags::empty(),
        0,
        16 * 1024 * 1024,
    );

    if !backing_supported {
        // Backing fs can't fallocate; yolofs must relay EOPNOTSUPP unchanged
        // rather than swallowing it or reporting a different error.
        assert_eq!(
            res,
            Err(Errno::EOPNOTSUPP),
            "yolofs should pass through the backing fs's EOPNOTSUPP verbatim"
        );
        return;
    }

    res.expect("fallocate through yolofs mount");
    drop(file);

    let st = fs::metadata(&path).expect("stat allocated file");
    assert_eq!(st.size(), 16 * 1024 * 1024);
    assert!(st.blocks() > 0, "fallocate should allocate blocks");

    let status = s.cli(&["review"]).expect("status");
    assert!(
        status.contains("fallocate.bin"),
        "status should include fallocate-created file: {status}"
    );
}

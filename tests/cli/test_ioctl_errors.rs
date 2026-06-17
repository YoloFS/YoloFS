use crate::helpers::YoloSession;
use std::fs;
use std::fs::OpenOptions;

/// Snapshot must fail with EBUSY while a staged file is held open.
#[test]
fn snapshot_rejects_while_staging_fd_open() {
    let s = YoloSession::new().expect("session setup");

    // Write creates a staged file (inode store copy).
    fs::write(s.mnt_path("hello.txt"), "modified\n").expect("write");

    // Hold a write fd open — this increments staging_fd_count.
    let _fd = OpenOptions::new()
        .write(true)
        .open(s.mnt_path("hello.txt"))
        .expect("open staged file for write");

    let (ok, _, stderr) = s.cli_output(&["snapshot", "should-fail"]).unwrap();
    assert!(!ok, "snapshot should fail while staging fd is open");
    assert!(
        stderr.contains("Device or resource busy"),
        "should report EBUSY: {stderr}"
    );

    // After closing the fd, snapshot should succeed.
    drop(_fd);
    s.cli(&["snapshot", "after-close"])
        .expect("snapshot should succeed after fd close");
}

/// Abort must fail with EBUSY while a staged file is held open.
#[test]
fn abort_rejects_while_staging_fd_open() {
    let s = YoloSession::new().expect("session setup");

    fs::write(s.mnt_path("hello.txt"), "modified\n").expect("write");

    let _fd = OpenOptions::new()
        .write(true)
        .open(s.mnt_path("hello.txt"))
        .expect("open staged file for write");

    let (ok, _, stderr) = s.cli_output(&["abort", "--force"]).unwrap();
    assert!(!ok, "abort should fail while staging fd is open");
    assert!(
        stderr.contains("Device or resource busy"),
        "should report EBUSY: {stderr}"
    );
    assert_eq!(
        fs::read_to_string(s.mnt_path("hello.txt")).unwrap(),
        "modified\n",
        "rejected abort must preserve the staged view"
    );
    assert!(
        fs::read_to_string(s.root.join(".yolofs/journal"))
            .unwrap()
            .contains("S\0/"),
        "rejected abort must preserve the artifact"
    );

    drop(_fd);
    s.cli(&["abort", "--force"])
        .expect("abort should succeed after fd close");
}

/// Travel must fail with EBUSY while a staged file is held open.
#[test]
fn travel_rejects_while_staging_fd_open() {
    let s = YoloSession::new().expect("session setup");

    fs::write(s.mnt_path("hello.txt"), "v1\n").expect("write v1");
    s.cli(&["snapshot", "v1"]).expect("snapshot v1");

    // Modify again and hold the fd open.
    fs::write(s.mnt_path("hello.txt"), "v2\n").expect("write v2");
    let _fd = OpenOptions::new()
        .write(true)
        .open(s.mnt_path("hello.txt"))
        .expect("open staged file for write");

    let (ok, _, stderr) = s.cli_output(&["travel", "1"]).unwrap();
    assert!(!ok, "travel should fail while staging fd is open");
    assert!(
        stderr.contains("Device or resource busy"),
        "should report EBUSY: {stderr}"
    );
    drop(_fd);
    s.cli(&["travel", "1"])
        .expect("travel should succeed after fd close");
}

/// Commit must fail with EBUSY while a staged file is held open.
#[test]
fn commit_rejects_while_staging_fd_open() {
    let s = YoloSession::new().expect("session setup");

    fs::write(s.mnt_path("hello.txt"), "modified\n").expect("write");

    let _fd = OpenOptions::new()
        .write(true)
        .open(s.mnt_path("hello.txt"))
        .expect("open staged file for write");

    let (ok, _, stderr) = s.cli_output(&["commit"]).unwrap();
    assert!(!ok, "commit should fail while staging fd is open");
    assert!(
        stderr.contains("Device or resource busy"),
        "should report EBUSY: {stderr}"
    );
    assert_eq!(
        fs::read_to_string(s.base_path("hello.txt")).unwrap(),
        "base content\n",
        "rejected commit must not change base"
    );
    assert!(
        fs::read_to_string(s.root.join(".yolofs/journal"))
            .unwrap()
            .contains("S\0/"),
        "rejected commit must preserve the artifact"
    );

    drop(_fd);
    s.cli(&["commit"])
        .expect("commit should succeed after fd close");
}

use crate::helpers::AgfsSession;
use std::fs;
use std::fs::OpenOptions;

/// Checkpoint must fail with EBUSY while a staged file is held open.
#[test]
fn checkpoint_rejects_while_staging_fd_open() {
    let s = AgfsSession::new().expect("session setup");
    s.run_in_namespace(|| {
        // Write creates a staged file (inode store copy).
        fs::write(s.mnt_path("hello.txt"), "modified\n").expect("write");

        // Hold a write fd open — this increments staging_fd_count.
        let _fd = OpenOptions::new()
            .write(true)
            .open(s.mnt_path("hello.txt"))
            .expect("open staged file for write");

        let (ok, _, stderr) = s.cli_output(&["checkpoint", "should-fail"]).unwrap();
        assert!(!ok, "checkpoint should fail while staging fd is open");
        assert!(
            stderr.contains("Device or resource busy"),
            "should report EBUSY: {stderr}"
        );

        // After closing the fd, checkpoint should succeed.
        drop(_fd);
        s.cli(&["checkpoint", "after-close"])
            .expect("checkpoint should succeed after fd close");
    });
}

/// Abort must fail with EBUSY while a staged file is held open.
#[test]
fn abort_rejects_while_staging_fd_open() {
    let s = AgfsSession::new().expect("session setup");
    s.run_in_namespace(|| {
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

        drop(_fd);
        s.cli(&["abort", "--force"])
            .expect("abort should succeed after fd close");
    });
}

/// Restore must fail with EBUSY while a staged file is held open.
#[test]
fn restore_rejects_while_staging_fd_open() {
    let s = AgfsSession::new().expect("session setup");
    s.run_in_namespace(|| {
        fs::write(s.mnt_path("hello.txt"), "v1\n").expect("write v1");
        s.cli(&["checkpoint", "v1"]).expect("checkpoint v1");

        // Modify again and hold the fd open.
        fs::write(s.mnt_path("hello.txt"), "v2\n").expect("write v2");
        let _fd = OpenOptions::new()
            .write(true)
            .open(s.mnt_path("hello.txt"))
            .expect("open staged file for write");

        let (ok, _, stderr) = s.cli_output(&["restore", "v1"]).unwrap();
        assert!(!ok, "restore should fail while staging fd is open");
        assert!(
            stderr.contains("Device or resource busy"),
            "should report EBUSY: {stderr}"
        );

        drop(_fd);
        s.cli(&["restore", "v1"])
            .expect("restore should succeed after fd close");
    });
}

/// Commit must fail with EBUSY while a staged file is held open.
#[test]
fn commit_rejects_while_staging_fd_open() {
    let s = AgfsSession::new().expect("session setup");
    s.run_in_namespace(|| {
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

        drop(_fd);
        s.cli(&["commit"])
            .expect("commit should succeed after fd close");
    });
}

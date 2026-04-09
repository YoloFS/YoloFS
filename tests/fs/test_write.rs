use crate::helpers::YoloSession;
use std::fs;
use std::fs::OpenOptions;
use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;

#[test]
fn write_triggers_cow() {
    let s = YoloSession::new().expect("session setup");

    fs::write(s.mnt_path("hello.txt"), "modified\n").expect("write through mount");

    // Read through mount sees new content
    let content = fs::read_to_string(s.mnt_path("hello.txt")).expect("read");
    assert_eq!(content, "modified\n");

    // Base file unchanged
    let base = fs::read_to_string(s.base_path("hello.txt")).expect("read base");
    assert_eq!(base, "base content\n");

    // The change should be visible via `yolofs status` and `yolofs diff`
    let status = s.cli(&["status"]).expect("status");
    assert!(
        status.contains("hello.txt"),
        "status should show modified file: {status}"
    );

    let diff = s.cli(&["diff"]).expect("diff");
    assert!(
        diff.contains("+modified"),
        "diff should show the new content: {diff}"
    );
}

#[test]
fn write_nested_file() {
    let s = YoloSession::new().expect("session setup");

    fs::write(s.mnt_path("subdir/deep.txt"), "changed\n").expect("write nested");

    let content = fs::read_to_string(s.mnt_path("subdir/deep.txt")).unwrap();
    assert_eq!(content, "changed\n");

    // Base unchanged
    let base = fs::read_to_string(s.base_path("subdir/deep.txt")).unwrap();
    assert_eq!(base, "nested\n");
}

#[test]
fn multiple_writes_same_file() {
    let s = YoloSession::new().expect("session setup");

    fs::write(s.mnt_path("hello.txt"), "first\n").unwrap();
    fs::write(s.mnt_path("hello.txt"), "second\n").unwrap();

    let content = fs::read_to_string(s.mnt_path("hello.txt")).unwrap();
    assert_eq!(content, "second\n");

    // Base still original
    assert_eq!(
        fs::read_to_string(s.base_path("hello.txt")).unwrap(),
        "base content\n"
    );
}

#[test]
fn sequential_writes_to_different_files() {
    let s = YoloSession::new().expect("session setup");

    fs::write(s.mnt_path("hello.txt"), "mod1\n").unwrap();
    fs::write(s.mnt_path("multi.txt"), "mod2\n").unwrap();
    fs::write(s.mnt_path("subdir/deep.txt"), "mod3\n").unwrap();

    assert_eq!(
        fs::read_to_string(s.mnt_path("hello.txt")).unwrap(),
        "mod1\n"
    );
    assert_eq!(
        fs::read_to_string(s.mnt_path("multi.txt")).unwrap(),
        "mod2\n"
    );
    assert_eq!(
        fs::read_to_string(s.mnt_path("subdir/deep.txt")).unwrap(),
        "mod3\n"
    );

    // All bases unchanged
    assert_eq!(
        fs::read_to_string(s.base_path("hello.txt")).unwrap(),
        "base content\n"
    );
    assert_eq!(
        fs::read_to_string(s.base_path("multi.txt")).unwrap(),
        "line1\nline2\n"
    );
    assert_eq!(
        fs::read_to_string(s.base_path("subdir/deep.txt")).unwrap(),
        "nested\n"
    );
}

#[test]
fn overwrite_then_read() {
    let s = YoloSession::new().expect("session setup");

    // Write, read, overwrite, read — should always see latest
    fs::write(s.mnt_path("hello.txt"), "v1\n").unwrap();
    assert_eq!(fs::read_to_string(s.mnt_path("hello.txt")).unwrap(), "v1\n");

    fs::write(s.mnt_path("hello.txt"), "v2\n").unwrap();
    assert_eq!(fs::read_to_string(s.mnt_path("hello.txt")).unwrap(), "v2\n");

    fs::write(s.mnt_path("hello.txt"), "v3 is longer content\n").unwrap();
    assert_eq!(
        fs::read_to_string(s.mnt_path("hello.txt")).unwrap(),
        "v3 is longer content\n"
    );
}

#[test]
fn append_multiple_times() {
    let s = YoloSession::new().expect("session setup");

    for i in 0..5 {
        let mut f = OpenOptions::new()
            .append(true)
            .open(s.mnt_path("multi.txt"))
            .unwrap();
        writeln!(f, "appended-{i}").unwrap();
    }

    let content = fs::read_to_string(s.mnt_path("multi.txt")).unwrap();
    assert!(content.starts_with("line1\nline2\n"));
    for i in 0..5 {
        assert!(
            content.contains(&format!("appended-{i}")),
            "missing appended-{i}"
        );
    }
}

// ── truncate and append ──

#[test]
fn truncating_write() {
    let s = YoloSession::new().expect("session setup");

    // Truncating write (O_TRUNC) via create-new
    fs::write(s.mnt_path("hello.txt"), "truncated\n").unwrap();

    assert_eq!(
        fs::read_to_string(s.mnt_path("hello.txt")).unwrap(),
        "truncated\n"
    );
    // Base unchanged
    assert_eq!(
        fs::read_to_string(s.base_path("hello.txt")).unwrap(),
        "base content\n"
    );
}

#[test]
fn append_to_file() {
    let s = YoloSession::new().expect("session setup");

    // Open for append
    let mut f = OpenOptions::new()
        .append(true)
        .open(s.mnt_path("multi.txt"))
        .expect("open for append");
    write!(f, "line3\n").expect("append");
    drop(f);

    let mut content = String::new();
    fs::File::open(s.mnt_path("multi.txt"))
        .unwrap()
        .read_to_string(&mut content)
        .unwrap();
    assert_eq!(content, "line1\nline2\nline3\n");

    // Base unchanged
    assert_eq!(
        fs::read_to_string(s.base_path("multi.txt")).unwrap(),
        "line1\nline2\n"
    );
}

/// Writing to a base file with read-only permissions still triggers COW
/// (the staged copy in the inode store is what gets written to).
#[test]
fn write_to_readonly_base_triggers_cow() {
    let s = YoloSession::new().expect("session setup");

    // Make the base file read-only
    let base = s.base_path("hello.txt");
    fs::set_permissions(&base, fs::Permissions::from_mode(0o444)).expect("chmod");

    // Writing through the mount should still succeed (COW to inode store)
    fs::write(s.mnt_path("hello.txt"), "overridden\n").expect("write to readonly base");

    let content = fs::read_to_string(s.mnt_path("hello.txt")).unwrap();
    assert_eq!(content, "overridden\n");

    // Restore permissions for cleanup
    fs::set_permissions(&base, fs::Permissions::from_mode(0o644)).expect("restore chmod");
}

/// COW should preserve the base file's permission mode, not reset to 0644.
#[test]
fn cow_preserves_readonly_mode() {
    use std::os::unix::fs::MetadataExt;

    let s = YoloSession::new().expect("session setup");

    let base = s.base_path("hello.txt");
    fs::set_permissions(&base, fs::Permissions::from_mode(0o444)).expect("chmod");

    fs::write(s.mnt_path("hello.txt"), "overridden\n").expect("write");

    let meta = fs::metadata(s.mnt_path("hello.txt")).unwrap();
    assert_eq!(
        meta.mode() & 0o777,
        0o444,
        "stat through mount should show original mode 0444, got {:o}",
        meta.mode() & 0o777
    );

    fs::set_permissions(&base, fs::Permissions::from_mode(0o644)).expect("restore");
}

/// COW should preserve executable mode bits.
#[test]
fn cow_preserves_executable_mode() {
    use std::os::unix::fs::MetadataExt;

    let s = YoloSession::new().expect("session setup");

    // test.sh is seeded with mode 0o755
    fs::write(s.mnt_path("test.sh"), "#!/bin/sh\necho modified\n").expect("write");

    let meta = fs::metadata(s.mnt_path("test.sh")).unwrap();
    assert_eq!(
        meta.mode() & 0o777,
        0o755,
        "stat should preserve executable mode 0755, got {:o}",
        meta.mode() & 0o777
    );
}

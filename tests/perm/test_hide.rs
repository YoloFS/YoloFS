use crate::helpers::{AGFS_BIN, AgfsSession};
use agfs::config::{Config, Perm};
use std::collections::BTreeMap;
use std::fs;

/// Helper: create a session with a hide rule on a subdirectory.
fn hide_session() -> AgfsSession {
    // Create base with a "secret" subdirectory.
    let root = tempfile::tempdir().unwrap().keep();
    fs::write(root.join("hello.txt"), "visible\n").unwrap();
    fs::create_dir_all(root.join("secret")).unwrap();
    fs::write(root.join("secret/key.pem"), "private\n").unwrap();
    fs::write(root.join("secret/notes.txt"), "hidden notes\n").unwrap();
    fs::create_dir_all(root.join("subdir")).unwrap();
    fs::write(root.join("subdir/deep.txt"), "nested\n").unwrap();
    fs::write(root.join("test.sh"), "#!/bin/sh\necho ok\n").unwrap();

    let mut rules = BTreeMap::new();
    rules.insert(root.join("secret").to_string_lossy().into_owned(), Perm::Hide);

    let config = Config {
        permission: true,
        ask_default: Some(Perm::Allow),
        rules,
        ..Default::default()
    };
    config.save(&root.join("agfs.toml")).unwrap();

    let output = std::process::Command::new(AGFS_BIN)
        .arg("mount")
        .current_dir(&root)
        .env("NO_COLOR", "1")
        .output()
        .unwrap();
    assert!(output.status.success(), "mount failed: {:?}", output);

    // Construct an AgfsSession manually so Drop unmounts.
    AgfsSession::from_existing_root(root).expect("session from root")
}

/// stat on a hidden path should return ENOENT.
#[test]
fn stat_hidden_returns_enoent() {
    let s = hide_session();
    let result = fs::metadata(s.mnt_path("secret"));
    assert!(result.is_err(), "stat on hidden dir should fail");
}

/// stat on a file inside a hidden dir should return ENOENT.
#[test]
fn stat_hidden_child_returns_enoent() {
    let s = hide_session();
    let result = fs::metadata(s.mnt_path("secret/key.pem"));
    assert!(result.is_err(), "stat on file in hidden dir should fail");
}

/// readdir on the parent should not list the hidden entry.
#[test]
fn readdir_skips_hidden_entry() {
    let s = hide_session();
    let entries: Vec<String> = fs::read_dir(s.mnt_path("."))
        .expect("readdir should succeed on parent")
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    assert!(
        !entries.contains(&"secret".to_string()),
        "readdir should not list 'secret': {entries:?}"
    );
    assert!(
        entries.contains(&"hello.txt".to_string()),
        "readdir should still list non-hidden files: {entries:?}"
    );
}

/// Reading a file in a hidden dir should fail.
#[test]
fn read_hidden_file_fails() {
    let s = hide_session();
    let result = fs::read_to_string(s.mnt_path("secret/key.pem"));
    assert!(result.is_err(), "reading hidden file should fail");
}

/// Creating a file in a hidden dir should fail.
#[test]
fn create_in_hidden_dir_fails() {
    let s = hide_session();
    let result = fs::write(s.mnt_path("secret/new.txt"), "data");
    assert!(result.is_err(), "creating in hidden dir should fail");
}

/// Non-hidden paths should work normally.
#[test]
fn non_hidden_path_works() {
    let s = hide_session();
    let content = fs::read_to_string(s.mnt_path("hello.txt"))
        .expect("reading non-hidden file should succeed");
    assert_eq!(content, "visible\n");
}

/// opendir on hidden dir should fail.
#[test]
fn opendir_hidden_fails() {
    let s = hide_session();
    let result = fs::read_dir(s.mnt_path("secret"));
    assert!(result.is_err(), "opendir on hidden dir should fail");
}

/// Hide on a single file (not a directory).
#[test]
fn hide_single_file() {
    let root = tempfile::tempdir().unwrap().keep();
    fs::write(root.join("visible.txt"), "ok\n").unwrap();
    fs::write(root.join("hidden.txt"), "secret\n").unwrap();
    fs::create_dir_all(root.join("subdir")).unwrap();
    fs::write(root.join("subdir/deep.txt"), "nested\n").unwrap();
    fs::write(root.join("test.sh"), "#!/bin/sh\n").unwrap();

    let mut rules = BTreeMap::new();
    rules.insert(
        root.join("hidden.txt").to_string_lossy().into_owned(),
        Perm::Hide,
    );

    let config = Config {
        permission: true,
        ask_default: Some(Perm::Allow),
        rules,
        ..Default::default()
    };
    config.save(&root.join("agfs.toml")).unwrap();

    let output = std::process::Command::new(AGFS_BIN)
        .arg("mount")
        .current_dir(&root)
        .env("NO_COLOR", "1")
        .output()
        .unwrap();
    assert!(output.status.success(), "mount failed");

    let s = AgfsSession::from_existing_root(root).expect("session");

    // hidden.txt should not appear in readdir.
    let entries: Vec<String> = fs::read_dir(s.mnt_path("."))
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    assert!(
        !entries.contains(&"hidden.txt".to_string()),
        "hidden file should not appear in readdir: {entries:?}"
    );
    assert!(entries.contains(&"visible.txt".to_string()));

    // stat should fail.
    assert!(fs::metadata(s.mnt_path("hidden.txt")).is_err());

    // read should fail.
    assert!(fs::read_to_string(s.mnt_path("hidden.txt")).is_err());

    // visible.txt should work.
    assert_eq!(
        fs::read_to_string(s.mnt_path("visible.txt")).unwrap(),
        "ok\n"
    );
}

/// Rename targeting a hidden path should fail.
#[test]
fn rename_to_hidden_fails() {
    let s = hide_session();
    let result = fs::rename(s.mnt_path("hello.txt"), s.mnt_path("secret/moved.txt"));
    assert!(result.is_err(), "rename into hidden dir should fail");
}

/// Unlink inside a hidden dir should fail.
#[test]
fn unlink_in_hidden_fails() {
    let s = hide_session();
    let result = fs::remove_file(s.mnt_path("secret/key.pem"));
    assert!(result.is_err(), "unlink in hidden dir should fail");
}

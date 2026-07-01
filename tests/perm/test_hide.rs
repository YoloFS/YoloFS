use crate::helpers::{YOLO_BIN, YoloSession};
use std::collections::BTreeMap;
use std::fs;
use yolofs::config::Config;
use yolofs::perm::Perm;

/// Helper: create a session with a hidden rule on a subdirectory.
fn hide_session() -> YoloSession {
    // Create base with a "secret" subdirectory.
    let root = crate::helpers::session_tempdir().unwrap().keep();
    fs::write(root.join("hello.txt"), "visible\n").unwrap();
    fs::create_dir_all(root.join("secret")).unwrap();
    fs::write(root.join("secret/key.pem"), "private\n").unwrap();
    fs::write(root.join("secret/notes.txt"), "hidden notes\n").unwrap();
    fs::create_dir_all(root.join("subdir")).unwrap();
    fs::write(root.join("subdir/deep.txt"), "nested\n").unwrap();
    fs::write(root.join("test.sh"), "#!/bin/sh\necho ok\n").unwrap();

    let mut rules = BTreeMap::new();
    rules.insert(
        root.join("secret").to_string_lossy().into_owned(),
        Perm::Hide,
    );

    // Permissive backdrop so non-hidden paths work; specific rules above win.
    rules.insert("/".into(), Perm::Allow);
    let config = Config {
        permission: true,
        rules,
        ..Default::default()
    };
    config.save(&root.join("yolofs.toml")).unwrap();

    let output = std::process::Command::new(YOLO_BIN)
        .arg("mount")
        .current_dir(&root)
        .env("NO_COLOR", "1")
        .output()
        .unwrap();
    assert!(output.status.success(), "mount failed: {:?}", output);

    // Construct an YoloSession manually so Drop unmounts.
    YoloSession::from_existing_root(root).expect("session from root")
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

/// opendir on hidden dir should fail with ENOENT.
#[test]
fn opendir_hidden_fails() {
    let s = hide_session();
    let result = fs::read_dir(s.mnt_path("secret"));
    assert!(result.is_err(), "opendir on hidden dir should fail");
    assert_eq!(
        result.unwrap_err().kind(),
        std::io::ErrorKind::NotFound,
        "opendir on hidden dir should return ENOENT"
    );
}

/// Hide on a single file (not a directory).
#[test]
fn hide_single_file() {
    let root = crate::helpers::session_tempdir().unwrap().keep();
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

    // Permissive backdrop so non-hidden paths work; specific rules above win.
    rules.insert("/".into(), Perm::Allow);
    let config = Config {
        permission: true,
        rules,
        ..Default::default()
    };
    config.save(&root.join("yolofs.toml")).unwrap();

    let output = std::process::Command::new(YOLO_BIN)
        .arg("mount")
        .current_dir(&root)
        .env("NO_COLOR", "1")
        .output()
        .unwrap();
    assert!(output.status.success(), "mount failed");

    let s = YoloSession::from_existing_root(root).expect("session");

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

/// readdir phase 1 (staged entries) and phase 2 (base entries) must agree:
/// in one directory, staged names appear, base names overridden by staged
/// entries or tombstones appear exactly once or not at all, and hidden base
/// names stay hidden.
#[test]
fn readdir_hides_entry_alongside_staged_siblings() {
    let s = hide_session();

    // Stage a brand-new file, overwrite a base file (staged override of a
    // base name), and delete another base file (pinned tombstone).
    fs::write(s.mnt_path("staged.txt"), "new\n").expect("create staged file");
    fs::write(s.mnt_path("hello.txt"), "overwritten\n").expect("overwrite base file");
    fs::remove_file(s.mnt_path("test.sh")).expect("delete base file");

    let entries: Vec<String> = fs::read_dir(s.mnt_path("."))
        .expect("readdir should succeed")
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();

    assert!(
        !entries.contains(&"secret".to_string()),
        "hidden entry must stay hidden with staged siblings present: {entries:?}"
    );
    assert!(
        !entries.contains(&"test.sh".to_string()),
        "deleted entry must not reappear: {entries:?}"
    );
    assert!(
        entries.contains(&"staged.txt".to_string()),
        "staged file must be listed: {entries:?}"
    );
    assert_eq!(
        entries.iter().filter(|e| *e == "hello.txt").count(),
        1,
        "overwritten base name must appear exactly once: {entries:?}"
    );
}

/// A hide rule added live (after mount, via the rule ioctl) must take effect
/// in readdir, and must survive a lookup/stat of the hidden name in between
/// (the rule dentry must stay findable in the dcache).
#[test]
fn live_hide_rule_hides_from_readdir() {
    let s = YoloSession::new_with_config(Config {
        rules: BTreeMap::from([("/".into(), Perm::Allow)]),
        ..Default::default()
    })
    .expect("session setup");

    // Visible before the rule.
    fs::metadata(s.mnt_path("hello.txt")).expect("stat should succeed before hide");

    s.cli(&[
        "rule",
        "hide",
        &s.root.join("hello.txt").display().to_string(),
    ])
    .unwrap();

    // stat (lookup path) sees ENOENT.
    assert!(
        fs::metadata(s.mnt_path("hello.txt")).is_err(),
        "stat should fail after live hide rule"
    );

    // readdir omits it — including after the stat above touched the name.
    for round in 0..2 {
        let entries: Vec<String> = fs::read_dir(s.mnt_path("."))
            .expect("readdir should succeed")
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert!(
            !entries.contains(&"hello.txt".to_string()),
            "live-hidden file listed in readdir round {round}: {entries:?}"
        );
        // Touch the name again between rounds.
        let _ = fs::metadata(s.mnt_path("hello.txt"));
    }
}

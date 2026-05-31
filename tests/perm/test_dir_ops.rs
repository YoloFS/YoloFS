use crate::helpers::YoloSession;
use std::collections::BTreeMap;
use std::fs;
use yolofs::config::Config;
use yolofs::perm::Perm;

// ── Directory read-like ops (stat, readdir, lookup/traversal) are NOT
// permission-gated — only hidden applies.  Mutations (mkdir, unlink,
// rmdir, rename, symlink) ARE gated via the parent directory's perm. ──

/// mkdir should fail under deny.
#[test]
fn mkdir_denied_under_deny() {
    let s = YoloSession::new_with_config(Config {
        ask_default: Some(Perm::Deny),
        rules: BTreeMap::from([("/".into(), Perm::Deny)]),
        ..Default::default()
    })
    .expect("session setup");

    let result = fs::create_dir(s.mnt_path("newdir"));
    assert!(result.is_err(), "mkdir should be denied");
}

/// unlink should fail under ro (needs write on parent dir).
#[test]
fn unlink_denied_under_ro() {
    let s = YoloSession::new_with_config(Config {
        ask_default: Some(Perm::Deny),
        rules: BTreeMap::from([("/".into(), Perm::Read)]),
        ..Default::default()
    })
    .expect("session setup");

    let result = fs::remove_file(s.mnt_path("hello.txt"));
    assert!(result.is_err(), "unlink should be denied under ro");
}

/// symlink creation should fail under deny.
#[test]
fn symlink_denied_under_deny() {
    let s = YoloSession::new_with_config(Config {
        ask_default: Some(Perm::Deny),
        rules: BTreeMap::from([("/".into(), Perm::Deny)]),
        ..Default::default()
    })
    .expect("session setup");

    let result = std::os::unix::fs::symlink("hello.txt", s.mnt_path("link.txt"));
    assert!(result.is_err(), "symlink should be denied");
}

/// rmdir should fail under deny.
#[test]
fn rmdir_denied_under_deny() {
    let s = YoloSession::new_with_config(Config {
        ask_default: Some(Perm::Deny),
        rules: BTreeMap::from([("/".into(), Perm::Deny)]),
        ..Default::default()
    })
    .expect("session setup");

    let result = fs::remove_dir(s.mnt_path("subdir"));
    assert!(result.is_err(), "rmdir should be denied");
}

/// rename should fail under deny.
#[test]
fn rename_denied_under_deny() {
    let s = YoloSession::new_with_config(Config {
        ask_default: Some(Perm::Deny),
        rules: BTreeMap::from([("/".into(), Perm::Deny)]),
        ..Default::default()
    })
    .expect("session setup");

    let result = fs::rename(s.mnt_path("hello.txt"), s.mnt_path("renamed.txt"));
    assert!(result.is_err(), "rename should be denied");
}

/// File creation should fail under deny (both dir op and open are gated).
#[test]
fn create_denied_under_deny() {
    let s = YoloSession::new_with_config(Config {
        ask_default: Some(Perm::Deny),
        rules: BTreeMap::from([("/".into(), Perm::Deny)]),
        ..Default::default()
    })
    .expect("session setup");

    let result = fs::write(s.mnt_path("newfile.txt"), "data");
    assert!(result.is_err(), "write to new file should fail under deny");
    assert!(
        !s.mnt_path("newfile.txt").exists(),
        "file should not exist after denied create"
    );
}

/// Listing a directory's contents should work even under deny
/// (readdir is not a mutation, yolo_readdir doesn't check perm).
#[test]
fn readdir_allowed_under_deny() {
    let s = YoloSession::new_with_config(Config {
        ask_default: Some(Perm::Deny),
        rules: BTreeMap::from([("/".into(), Perm::Deny)]),
        ..Default::default()
    })
    .expect("session setup");

    let entries: Vec<_> = fs::read_dir(s.mnt_path("."))
        .expect("readdir should succeed")
        .filter_map(|e| e.ok())
        .collect();
    assert!(
        !entries.is_empty(),
        "readdir should see base files even under deny"
    );
}

/// Stat should work even under deny (stat doesn't go through yolo_open).
#[test]
fn stat_allowed_under_deny() {
    let s = YoloSession::new_with_config(Config {
        ask_default: Some(Perm::Deny),
        rules: BTreeMap::from([("/".into(), Perm::Deny)]),
        ..Default::default()
    })
    .expect("session setup");

    let meta = fs::metadata(s.mnt_path("hello.txt"));
    assert!(
        meta.is_ok(),
        "stat should succeed (no open needed): {:?}",
        meta.err()
    );
}

/// readdir is not permission-gated. Even on an ask directory with
/// ask_default=deny, readdir succeeds because it is never gated.
#[test]
fn readdir_on_ask_dir_still_succeeds_when_default_denies() {
    let s = YoloSession::new_with_config(Config {
        ask_default: Some(Perm::Deny),
        rules: BTreeMap::from([("/".into(), Perm::Allow)]),
        ..Default::default()
    })
    .expect("session setup");
    s.cli(&["rule", "ask", "subdir"]).unwrap();

    let entries: Vec<_> = fs::read_dir(s.mnt_path("subdir"))
        .expect("readdir should succeed")
        .filter_map(|e| e.ok())
        .collect();
    assert!(
        !entries.is_empty(),
        "readdir should list entries in ask dir even when ask_default=deny"
    );
}

// ── Verify directory read-like ops are NOT gated ──
//
// These tests use no explicit rules (everything defaults to ask) and no
// daemon, with ask_default=deny.  If stat/readdir/lookup went through the
// ask path they would resolve to deny and fail.  Their success proves
// that directory read-like ops bypass the ask path entirely.

/// Stat on a directory is not gated.
#[test]
fn stat_on_ask_dir_not_gated() {
    let s = YoloSession::new_with_config(Config {
        ask_default: Some(Perm::Deny),
        rules: BTreeMap::new(),
        ..Default::default()
    })
    .expect("session setup");

    let meta = fs::metadata(s.mnt_path("subdir"));
    assert!(
        meta.is_ok(),
        "stat on directory should not be gated: {:?}",
        meta.err()
    );
}

/// Readdir on a directory is not gated.
#[test]
fn readdir_on_ask_dir_not_gated() {
    let s = YoloSession::new_with_config(Config {
        ask_default: Some(Perm::Deny),
        rules: BTreeMap::new(),
        ..Default::default()
    })
    .expect("session setup");

    let entries: Vec<_> = fs::read_dir(s.mnt_path("subdir"))
        .expect("readdir should not be gated")
        .filter_map(|e| e.ok())
        .collect();
    assert!(!entries.is_empty(), "readdir should list entries");
}

/// Path lookup/traversal through directories is not gated. Stat on a
/// nested file succeeds because traversal + stat are both ungated.
#[test]
fn lookup_traversal_not_gated() {
    let s = YoloSession::new_with_config(Config {
        ask_default: Some(Perm::Deny),
        rules: BTreeMap::new(),
        ..Default::default()
    })
    .expect("session setup");

    let meta = fs::metadata(s.mnt_path("subdir/deep.txt"));
    assert!(
        meta.is_ok(),
        "stat on nested file should succeed (traversal + stat not gated): {:?}",
        meta.err()
    );
}

/// File open IS still gated even though dir ops are not.
#[test]
fn file_open_still_gated_when_dir_not_gated() {
    let s = YoloSession::new_with_config(Config {
        ask_default: Some(Perm::Deny),
        rules: BTreeMap::new(),
        ..Default::default()
    })
    .expect("session setup");

    // Traversal + stat succeeds (not gated)
    assert!(
        fs::metadata(s.mnt_path("subdir/deep.txt")).is_ok(),
        "stat should succeed"
    );
    // But reading the file fails (file open IS gated → ask → deny)
    assert!(
        fs::read_to_string(s.mnt_path("subdir/deep.txt")).is_err(),
        "file read should be denied (open is still gated)"
    );
}

/// mkdir inside an ask directory is gated (ask resolves to deny with no daemon).
#[test]
fn mkdir_in_ask_dir_gated() {
    let s = YoloSession::new_with_config(Config {
        ask_default: Some(Perm::Deny),
        rules: BTreeMap::new(),
        ..Default::default()
    })
    .expect("session setup");

    // Traversal succeeds (not gated)
    assert!(
        fs::metadata(s.mnt_path("subdir")).is_ok(),
        "stat should succeed"
    );
    // But mkdir fails (mutation IS gated via parent perm → ask → deny)
    let result = fs::create_dir(s.mnt_path("subdir/newchild"));
    assert!(
        result.is_err(),
        "mkdir should be denied (mutations are still gated)"
    );
}

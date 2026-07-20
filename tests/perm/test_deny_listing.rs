//! `deny` on a directory blocks *listing* its contents (readdir), while still
//! permitting traversal to explicitly-allowed children (nearest-ancestor wins).
//! This is the enumeration-control behavior that replaced the old `hide`
//! policy. Content protection is unchanged: non-allowed files under the deny
//! stay unreadable.

use crate::helpers::{YOLO_BIN, YoloSession};
use std::collections::BTreeMap;
use std::fs;
use yolofs::config::Config;
use yolofs::perm::Perm;

/// Session with `deny d/`, an explicit `allow d/ok.txt`, and an allow backdrop.
fn deny_listing_session() -> YoloSession {
    let root = crate::helpers::session_tempdir().unwrap().keep();
    fs::create_dir_all(root.join("d")).unwrap();
    fs::write(root.join("d/ok.txt"), "reachable\n").unwrap();
    fs::write(root.join("d/secret.txt"), "protected\n").unwrap();
    fs::write(root.join("top.txt"), "top\n").unwrap();

    let mut rules = BTreeMap::new();
    rules.insert("/".into(), Perm::Allow); // backdrop
    rules.insert(root.join("d").to_string_lossy().into_owned(), Perm::Deny);
    rules.insert(
        root.join("d/ok.txt").to_string_lossy().into_owned(),
        Perm::Allow,
    );
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
    assert!(output.status.success(), "mount failed: {output:?}");
    YoloSession::from_existing_root(root).expect("session from root")
}

/// Listing a `deny`d directory fails with EACCES.
#[test]
fn deny_dir_blocks_listing() {
    let s = deny_listing_session();
    // Opening the dir fd succeeds (control ioctls need that); the block is on
    // enumerating entries (getdents), so it surfaces on iteration.
    let listed: std::io::Result<Vec<_>> = fs::read_dir(s.mnt_path("d"))
        .expect("opening the dir fd should succeed")
        .collect();
    assert!(listed.is_err(), "listing a deny'd dir must fail");
    assert_eq!(
        listed.unwrap_err().kind(),
        std::io::ErrorKind::PermissionDenied,
        "listing a deny'd dir should be EACCES"
    );
}

/// An explicitly-allowed child under a `deny`d dir stays reachable by name
/// (traversal is not blocked — nearest-ancestor wins gives the child `allow`).
#[test]
fn deny_dir_allows_traversal_to_allowed_child() {
    let s = deny_listing_session();
    let content = fs::read_to_string(s.mnt_path("d/ok.txt"))
        .expect("allowed child under a deny'd dir must be readable");
    assert_eq!(content, "reachable\n");
}

/// A non-allowed file under a `deny`d dir is still unreadable (content
/// protection unchanged).
#[test]
fn deny_dir_denies_non_allowed_child_read() {
    let s = deny_listing_session();
    assert!(
        fs::read_to_string(s.mnt_path("d/secret.txt")).is_err(),
        "a non-allowed file under deny must not be readable"
    );
}

/// Accepted tradeoff vs the old `hide`: the deny'd directory's own name
/// remains visible in its parent's listing (only its *contents* are blocked).
#[test]
fn deny_dir_name_stays_visible_in_parent() {
    let s = deny_listing_session();
    let names: Vec<String> = fs::read_dir(s.mnt_path("."))
        .expect("listing the session root should succeed")
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    assert!(
        names.contains(&"d".to_string()),
        "deny'd dir name should stay visible in its parent: {names:?}"
    );
}

/// Listing must be blocked before the staging merge: a deny'd dir that also
/// has a staged (newly created) child must still return EACCES on readdir,
/// not leak a partial staged listing. (The check short-circuits ahead of the
/// staged+base merge in yolo_readdir.)
#[test]
fn deny_dir_with_staged_child_still_blocks_listing() {
    let s = deny_listing_session();
    // Create a staged child under the deny'd dir via the explicitly-allowed
    // name (create is gated on the parent, so use the allowed child path).
    fs::write(s.mnt_path("d/ok.txt"), "restaged\n").expect("write allowed child");

    let listed: std::io::Result<Vec<_>> = fs::read_dir(s.mnt_path("d"))
        .expect("opening the dir fd should succeed")
        .collect();
    assert!(
        listed.is_err(),
        "listing a deny'd dir with a staged child must still be blocked"
    );
    assert_eq!(
        listed.unwrap_err().kind(),
        std::io::ErrorKind::PermissionDenied
    );
}

/// A live rule flip to/from `deny` must take effect on directory listing (the
/// generation bump invalidates cached_access as consumed by yolo_readdir).
#[test]
fn live_deny_toggles_listing() {
    let s = YoloSession::new_with_config(Config {
        rules: BTreeMap::from([("/".into(), Perm::Allow)]),
        ..Default::default()
    })
    .expect("session setup");

    // Listable under allow.
    let ok: std::io::Result<Vec<_>> = fs::read_dir(s.mnt_path("subdir"))
        .expect("opendir")
        .collect();
    assert!(ok.is_ok(), "listing should work under allow");

    // Deny live → listing blocked.
    s.cli(&["rule", "deny", &s.root.join("subdir").display().to_string()])
        .expect("set live deny");
    let blocked: std::io::Result<Vec<_>> = fs::read_dir(s.mnt_path("subdir"))
        .expect("opendir still succeeds")
        .collect();
    assert!(
        blocked.is_err(),
        "listing should be blocked after live deny"
    );

    // Unset live → listing restored.
    s.cli(&[
        "rule",
        "unset",
        &s.root.join("subdir").display().to_string(),
    ])
    .expect("unset live");
    let restored: std::io::Result<Vec<_>> = fs::read_dir(s.mnt_path("subdir"))
        .expect("opendir")
        .collect();
    assert!(restored.is_ok(), "listing should work again after unset");
}

/// `read-only` and `write-ask` directories still list normally — only `deny`
/// blocks enumeration. (Guards against a regression that gates readdir on any
/// non-allow policy.)
#[test]
fn non_deny_dirs_still_list() {
    for policy in ["read-only", "write-ask"] {
        let s = YoloSession::new_with_config(Config {
            rules: BTreeMap::from([("/".into(), Perm::Allow)]),
            ..Default::default()
        })
        .expect("session setup");
        s.cli(&["rule", policy, &s.root.join("subdir").display().to_string()])
            .unwrap_or_else(|e| panic!("set {policy}: {e}"));
        let listed: std::io::Result<Vec<_>> = fs::read_dir(s.mnt_path("subdir"))
            .expect("opendir")
            .collect();
        assert!(listed.is_ok(), "a {policy} dir should still list");
    }
}

/// The control ioctls live on a mount directory fd, which the CLI opens to send
/// RULE_SET. Blocking `deny` at getdents (not at open) is precisely so this
/// keeps working: with `/`=deny you must still be able to `yolo rule` the root
/// to un-deny it. Regression guard for the getdents-not-open design.
#[test]
fn control_ioctl_works_under_root_deny() {
    let s = YoloSession::new_with_config(Config {
        rules: BTreeMap::from([("/".into(), Perm::Deny)]),
        ..Default::default()
    })
    .expect("session setup");

    // Root read is denied...
    assert!(
        fs::read_to_string(s.mnt_path("hello.txt")).is_err(),
        "read should be denied under /=deny"
    );
    // ...but the control ioctl (which opens the mount-root dir fd) still works,
    // so we can lift the deny live.
    s.cli(&["rule", "allow", &s.root.display().to_string()])
        .expect("rule ioctl must work under a root deny (dir fd still opens)");
    assert_eq!(
        fs::read_to_string(s.mnt_path("hello.txt")).expect("read after allow"),
        "base content\n"
    );
}

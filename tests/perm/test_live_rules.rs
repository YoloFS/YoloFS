use crate::helpers::{AGFS_BIN, AgfsSession};
use agfs::config::{Config, Perm};
use std::collections::BTreeMap;
use std::fs;

// ── Newly created files respect permissions ──

/// A file created inside the sandbox should still be subject to perm gating
/// when reopened. The perm_gen fix ensures new inodes get re-resolved.
#[test]
fn newly_created_file_checked_on_reopen() {
    let mut s = AgfsSession::new_with_config(Config {
        ask_default: Some(Perm::Deny),
        rules: BTreeMap::from([("/".into(), Perm::AllowRw)]),
        ..Default::default()
    })
    .expect("session setup");

    // Phase 1: create a file in the sandbox
    s.run_in_namespace(|| {
        fs::write(s.mnt_path("newfile.txt"), "hello").expect("create should succeed");
    });

    // Phase 2: unmount + change rules + remount (from host)
    s.cli(&["unmount", "--force"]).unwrap();
    Config {
        ask_default: Some(Perm::Deny),
        rules: BTreeMap::from([("/".into(), Perm::Deny)]),
        ..Default::default()
    }
    .save(&s.root.join("agfs.toml"))
    .unwrap();
    let output = std::process::Command::new(AGFS_BIN)
        .arg("mount")
        .current_dir(&s.root)
        .env("NO_COLOR", "1")
        .output()
        .expect("remount");
    assert!(output.status.success(), "remount failed");
    s.refresh_daemon_pid().expect("refresh pid after remount");

    // Phase 3: verify deny takes effect (in new namespace)
    s.run_in_namespace(|| {
        let result = fs::read_to_string(s.mnt_path("hello.txt"));
        assert!(
            result.is_err(),
            "read should be denied after rule change to deny"
        );
    });
}

// ── Rule change via live ioctl (cache invalidation) ──

/// Changing rules at runtime via `agfs rule add` should take effect
/// on subsequent opens (perm_gen increment forces cache re-resolution).
#[test]
fn live_rule_change_takes_effect() {
    let s = AgfsSession::new_with_config(Config {
        ask_default: Some(Perm::Deny),
        rules: BTreeMap::from([("/".into(), Perm::Deny)]),
        ..Default::default()
    })
    .expect("session setup");

    s.run_in_namespace(|| {
        let result = fs::read_to_string(s.mnt_path("hello.txt"));
        assert!(result.is_err(), "read should fail under deny");

        // `rule add` takes a host path and resolves it through the mount internally.
        s.cli(&["rule", "add", &s.root.display().to_string(), "allow-rw"])
            .unwrap();

        let content = fs::read_to_string(s.mnt_path("hello.txt"))
            .expect("read should succeed after live rule add");
        assert_eq!(content, "base content\n");
    });
}

/// Removing a rule at runtime should re-gate access.
#[test]
fn live_rule_remove_reapplies_gating() {
    let s = AgfsSession::new_with_config(Config {
        ask_default: Some(Perm::Deny),
        rules: BTreeMap::new(),
        ..Default::default()
    })
    .expect("session setup");

    s.run_in_namespace(|| {
        s.cli(&["rule", "add", &s.root.display().to_string(), "allow-rw"])
            .unwrap();
        fs::read_to_string(s.mnt_path("hello.txt")).expect("read should succeed with allow-rw rule");

        s.cli(&["rule", "remove", &s.root.display().to_string()])
            .unwrap();
        let result = fs::read_to_string(s.mnt_path("hello.txt"));
        assert!(result.is_err(), "read should fail after rule removal");
    });
}

// ── Rename across permission boundaries ──

/// Renaming a file from an allowed dir to a denied dir should succeed (dir op),
/// but reading the renamed file in the denied dir should fail after a cache
/// invalidation.
#[test]
fn rename_across_permission_boundary() {
    let mut s = AgfsSession::new_with_config(Config {
        permission: false,
        rules: BTreeMap::new(),
        ..Default::default()
    })
    .expect("session setup");

    // Phase 1: create base files in the namespace
    s.run_in_namespace(|| {
        fs::create_dir_all(s.root.join("allowed")).expect("mkdir allowed");
        fs::create_dir_all(s.root.join("denied")).expect("mkdir denied");
        fs::write(s.root.join("allowed/file.txt"), "content\n").expect("create file");
    });

    // Phase 2: unmount + configure rules + remount (from host)
    s.cli(&["unmount"]).unwrap();
    Config {
        ask_default: Some(Perm::Deny),
        rules: BTreeMap::from([
            (s.root.join("allowed").display().to_string(), Perm::AllowRw),
            (s.root.join("denied").display().to_string(), Perm::Deny),
        ]),
        ..Default::default()
    }
    .save(&s.root.join("agfs.toml"))
    .unwrap();
    let output = std::process::Command::new(AGFS_BIN)
        .arg("mount")
        .current_dir(&s.root)
        .env("NO_COLOR", "1")
        .output()
        .expect("remount");
    assert!(output.status.success(), "remount failed");
    s.refresh_daemon_pid().expect("refresh pid after remount");

    // Phase 3: rename and verify denial (in new namespace)
    s.run_in_namespace(|| {
        fs::read_to_string(s.mnt_path("allowed/file.txt"))
            .expect("reading file in allowed dir should succeed");

        fs::rename(
            s.mnt_path("allowed/file.txt"),
            s.mnt_path("denied/file.txt"),
        )
        .expect("rename is a dir op and should succeed");

        // Force cache invalidation
        s.cli(&[
            "rule",
            "add",
            &s.root.join("denied").display().to_string(),
            "deny",
        ])
        .unwrap();

        let result = fs::read_to_string(s.mnt_path("denied/file.txt"));
        assert!(
            result.is_err(),
            "reading renamed file in denied dir should fail"
        );
    });
}

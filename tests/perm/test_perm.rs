use crate::helpers::{AGFS_BIN, AgfsSession};
use agfs::config::{Config, Perm};
use std::collections::BTreeMap;
use std::fs;

/// With no daemon and ask_default=deny, reading an unruled file should fail.
#[test]
fn no_daemon_denies_by_default() {
    let s = AgfsSession::new_with_config(Config {
        ask_default: Some(Perm::Deny),
        rules: BTreeMap::new(),
        ..Default::default()
    })
    .expect("session setup");

    let result = fs::read_to_string(s.mnt_path("hello.txt"));
    assert!(
        result.is_err(),
        "read should be denied without daemon or rule"
    );
}

/// With no daemon and ask_default=allow, reading an unruled file should succeed.
#[test]
fn no_daemon_allows_when_configured() {
    let s = AgfsSession::new_with_config(Config {
        ask_default: Some(Perm::Allow),
        rules: BTreeMap::new(),
        ..Default::default()
    })
    .expect("session setup");

    let content = fs::read_to_string(s.mnt_path("hello.txt"))
        .expect("read should succeed with ask_default=allow");
    assert_eq!(content, "base content\n");
}

/// An explicit allow rule should bypass the ask mechanism entirely.
#[test]
fn explicit_rule_bypasses_ask() {
    let s = AgfsSession::new_with_config(Config {
        ask_default: Some(Perm::Deny),
        rules: BTreeMap::from([("/".into(), Perm::Allow)]),
        ..Default::default()
    })
    .expect("session setup");

    let content = fs::read_to_string(s.mnt_path("hello.txt"))
        .expect("read should succeed with explicit allow rule");
    assert_eq!(content, "base content\n");
}

/// An explicit deny rule should block access even with permission=true.
#[test]
fn deny_rule_blocks_access() {
    let s = AgfsSession::new_with_config(Config {
        ask_default: Some(Perm::Allow),
        rules: BTreeMap::from([("/".into(), Perm::Deny)]),
        ..Default::default()
    })
    .expect("session setup");

    let result = fs::read_to_string(s.mnt_path("hello.txt"));
    assert!(
        result.is_err(),
        "read should be denied with explicit deny rule"
    );
}

/// With permission=false, everything is allowed regardless of rules.
#[test]
fn permission_disabled_allows_everything() {
    let s = AgfsSession::new_with_config(Config {
        permission: false,
        rules: BTreeMap::from([("/".into(), Perm::Deny)]),
        ..Default::default()
    })
    .expect("session setup");

    let content = fs::read_to_string(s.mnt_path("hello.txt"))
        .expect("read should succeed with permission=false even with deny rule");
    assert_eq!(content, "base content\n");
}

/// allow-ro should permit reads but deny writes.
#[test]
fn allow_ro_permits_read_denies_write() {
    let s = AgfsSession::new_with_config(Config {
        ask_default: Some(Perm::Deny),
        rules: BTreeMap::from([("/".into(), Perm::AllowRo)]),
        ..Default::default()
    })
    .expect("session setup");

    // Read should succeed
    let content =
        fs::read_to_string(s.mnt_path("hello.txt")).expect("read should succeed with allow-ro");
    assert_eq!(content, "base content\n");

    // Write should fail
    let result = fs::write(s.mnt_path("hello.txt"), "modified\n");
    assert!(result.is_err(), "write should be denied with allow-ro rule");
}

// ── allow-rw tests (perm.c: agfs_check_perm, inode.c: agfs_permission) ──

/// allow-rw should permit reads.
#[test]
fn allow_rw_permits_read() {
    let s = AgfsSession::new_with_config(Config {
        ask_default: Some(Perm::Deny),
        rules: BTreeMap::from([("/".into(), Perm::AllowRw)]),
        ..Default::default()
    })
    .expect("session setup");

    let content =
        fs::read_to_string(s.mnt_path("hello.txt")).expect("read should succeed with allow-rw");
    assert_eq!(content, "base content\n");
}

/// allow-rw should permit writes.
#[test]
fn allow_rw_permits_write() {
    let s = AgfsSession::new_with_config(Config {
        ask_default: Some(Perm::Deny),
        rules: BTreeMap::from([("/".into(), Perm::AllowRw)]),
        ..Default::default()
    })
    .expect("session setup");

    fs::write(s.mnt_path("hello.txt"), "modified\n").expect("write should succeed with allow-rw");
}

/// allow-rw should deny exec (MAY_EXEC check in agfs_permission).
#[test]
fn allow_rw_denies_exec() {
    let s = AgfsSession::new_with_config(Config {
        ask_default: Some(Perm::Deny),
        rules: BTreeMap::from([("/".into(), Perm::AllowRw)]),
        ..Default::default()
    })
    .expect("session setup");

    let result = std::process::Command::new(s.mnt_path("test.sh")).output();
    // execve should fail with EACCES (permission denied)
    assert!(
        result.is_err() || !result.unwrap().status.success(),
        "exec should be denied with allow-rw"
    );
}

// ── allow-rx tests (perm.c: agfs_check_perm, inode.c: agfs_permission) ──

/// allow-rx should permit reads.
#[test]
fn allow_rx_permits_read() {
    let s = AgfsSession::new_with_config(Config {
        ask_default: Some(Perm::Deny),
        rules: BTreeMap::from([("/".into(), Perm::AllowRx)]),
        ..Default::default()
    })
    .expect("session setup");

    let content =
        fs::read_to_string(s.mnt_path("hello.txt")).expect("read should succeed with allow-rx");
    assert_eq!(content, "base content\n");
}

/// allow-rx should deny writes.
#[test]
fn allow_rx_denies_write() {
    let s = AgfsSession::new_with_config(Config {
        ask_default: Some(Perm::Deny),
        rules: BTreeMap::from([("/".into(), Perm::AllowRx)]),
        ..Default::default()
    })
    .expect("session setup");

    let result = fs::write(s.mnt_path("hello.txt"), "modified\n");
    assert!(result.is_err(), "write should be denied with allow-rx rule");
}

/// allow-rx should permit exec.
#[test]
fn allow_rx_permits_exec() {
    let s = AgfsSession::new_with_config(Config {
        ask_default: Some(Perm::Deny),
        rules: BTreeMap::from([("/".into(), Perm::AllowRx)]),
        ..Default::default()
    })
    .expect("session setup");

    let output = std::process::Command::new(s.mnt_path("test.sh"))
        .output()
        .expect("should be able to spawn executable");
    assert!(output.status.success(), "exec should succeed with allow-rx");
}

// ── allow (full access) ──

/// allow should permit exec.
#[test]
fn allow_permits_exec() {
    let s = AgfsSession::new_with_config(Config {
        ask_default: Some(Perm::Deny),
        rules: BTreeMap::from([("/".into(), Perm::Allow)]),
        ..Default::default()
    })
    .expect("session setup");

    let output = std::process::Command::new(s.mnt_path("test.sh"))
        .output()
        .expect("should be able to spawn executable");
    assert!(output.status.success(), "exec should succeed with allow");
}

// ── deny (all blocked) ──

/// deny should block writes (not just reads).
#[test]
fn deny_blocks_write() {
    let s = AgfsSession::new_with_config(Config {
        ask_default: Some(Perm::Allow),
        rules: BTreeMap::from([("/".into(), Perm::Deny)]),
        ..Default::default()
    })
    .expect("session setup");

    let result = fs::write(s.mnt_path("hello.txt"), "modified\n");
    assert!(result.is_err(), "write should be denied with deny rule");
}

/// deny should block exec.
#[test]
fn deny_blocks_exec() {
    let s = AgfsSession::new_with_config(Config {
        ask_default: Some(Perm::Allow),
        rules: BTreeMap::from([("/".into(), Perm::Deny)]),
        ..Default::default()
    })
    .expect("session setup");

    let result = std::process::Command::new(s.mnt_path("test.sh")).output();
    assert!(
        result.is_err() || !result.unwrap().status.success(),
        "exec should be denied with deny rule"
    );
}

// ── Rule inheritance (perm.c: agfs_resolve_perm walks dentry chain) ──

/// A more specific (child) rule should override a broader (parent) rule.
/// "/" = deny, but session root = allow-rw → files in session are accessible.
#[test]
fn child_rule_overrides_parent() {
    let s = AgfsSession::new_with_config(Config {
        permission: false,
        rules: BTreeMap::new(),
        ..Default::default()
    })
    .expect("session setup");

    // Unmount, write config with root-path-dependent rules, remount
    s.cli(&["unmount"]).unwrap();
    let root_path = s.root.display().to_string();
    Config {
        ask_default: Some(Perm::Deny),
        rules: BTreeMap::from([("/".into(), Perm::Deny), (root_path, Perm::AllowRw)]),
        ..Default::default()
    }
    .save(&s.root.join("agfs.toml"))
    .unwrap();
    std::process::Command::new(crate::helpers::AGFS_BIN)
        .arg("mount")
        .current_dir(&s.root)
        .env("NO_COLOR", "1")
        .output()
        .expect("remount");

    let content = fs::read_to_string(s.mnt_path("hello.txt"))
        .expect("child allow-rw should override parent deny");
    assert_eq!(content, "base content\n");
}

// ── ask_default variants (perm.c: agfs_ask_userspace no-daemon path) ──

/// ask_default=allow-ro: read OK, write denied.
#[test]
fn ask_default_allow_ro() {
    let s = AgfsSession::new_with_config(Config {
        ask_default: Some(Perm::AllowRo),
        rules: BTreeMap::new(),
        ..Default::default()
    })
    .expect("session setup");

    let content = fs::read_to_string(s.mnt_path("hello.txt"))
        .expect("read should succeed with ask_default=allow-ro");
    assert_eq!(content, "base content\n");

    let result = fs::write(s.mnt_path("hello.txt"), "modified\n");
    assert!(
        result.is_err(),
        "write should be denied with ask_default=allow-ro"
    );
}

/// ask_default=allow-rw: read OK, write OK.
#[test]
fn ask_default_allow_rw() {
    let s = AgfsSession::new_with_config(Config {
        ask_default: Some(Perm::AllowRw),
        rules: BTreeMap::new(),
        ..Default::default()
    })
    .expect("session setup");

    let content = fs::read_to_string(s.mnt_path("hello.txt"))
        .expect("read should succeed with ask_default=allow-rw");
    assert_eq!(content, "base content\n");

    fs::write(s.mnt_path("hello.txt"), "modified\n")
        .expect("write should succeed with ask_default=allow-rw");
}

/// ask_default=allow-rx: read OK, write denied.
#[test]
fn ask_default_allow_rx() {
    let s = AgfsSession::new_with_config(Config {
        ask_default: Some(Perm::AllowRx),
        rules: BTreeMap::new(),
        ..Default::default()
    })
    .expect("session setup");

    let content = fs::read_to_string(s.mnt_path("hello.txt"))
        .expect("read should succeed with ask_default=allow-rx");
    assert_eq!(content, "base content\n");

    let result = fs::write(s.mnt_path("hello.txt"), "modified\n");
    assert!(
        result.is_err(),
        "write should be denied with ask_default=allow-rx"
    );
}

// ── Directory ops bypass agfs permission (inode.c: agfs_permission
//    delegates to lower FS for non-regular files) ──

/// mkdir should succeed even under a deny rule because directory ops
/// are checked against the lower FS, not agfs permission rules.
#[test]
fn mkdir_allowed_under_deny() {
    let s = AgfsSession::new_with_config(Config {
        ask_default: Some(Perm::Deny),
        rules: BTreeMap::from([("/".into(), Perm::Deny)]),
        ..Default::default()
    })
    .expect("session setup");

    fs::create_dir(s.mnt_path("newdir")).expect("mkdir should succeed: dir ops bypass agfs perm");
}

/// unlink should succeed under allow-ro because inode removal is a
/// directory operation and doesn't go through agfs_permission on the file.
#[test]
fn unlink_allowed_under_allow_ro() {
    let s = AgfsSession::new_with_config(Config {
        ask_default: Some(Perm::Deny),
        rules: BTreeMap::from([("/".into(), Perm::AllowRo)]),
        ..Default::default()
    })
    .expect("session setup");

    // unlink goes through agfs_unlink which adds a DELETED override
    fs::remove_file(s.mnt_path("hello.txt"))
        .expect("unlink should succeed: it is a dir op on the parent");
}

/// symlink creation should succeed under deny because symlink is a
/// directory inode operation.
#[test]
fn symlink_allowed_under_deny() {
    let s = AgfsSession::new_with_config(Config {
        ask_default: Some(Perm::Deny),
        rules: BTreeMap::from([("/".into(), Perm::Deny)]),
        ..Default::default()
    })
    .expect("session setup");

    std::os::unix::fs::symlink("hello.txt", s.mnt_path("link.txt"))
        .expect("symlink should succeed: dir ops bypass agfs perm");
}

// ── O_TRUNC treated as write ──

/// O_TRUNC counts as a write operation; allow-ro should deny it.
#[test]
fn truncate_denied_on_allow_ro() {
    let s = AgfsSession::new_with_config(Config {
        ask_default: Some(Perm::Deny),
        rules: BTreeMap::from([("/".into(), Perm::AllowRo)]),
        ..Default::default()
    })
    .expect("session setup");

    let result = fs::OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(s.mnt_path("hello.txt"));
    assert!(result.is_err(), "O_TRUNC should be denied with allow-ro");
}

/// O_APPEND counts as a write operation; allow-ro should deny it.
#[test]
fn append_denied_on_allow_ro() {
    let s = AgfsSession::new_with_config(Config {
        ask_default: Some(Perm::Deny),
        rules: BTreeMap::from([("/".into(), Perm::AllowRo)]),
        ..Default::default()
    })
    .expect("session setup");

    let result = fs::OpenOptions::new()
        .append(true)
        .open(s.mnt_path("hello.txt"));
    assert!(result.is_err(), "O_APPEND should be denied with allow-ro");
}

/// O_RDWR counts as a write; allow-ro should deny it.
#[test]
fn rdwr_denied_on_allow_ro() {
    let s = AgfsSession::new_with_config(Config {
        ask_default: Some(Perm::Deny),
        rules: BTreeMap::from([("/".into(), Perm::AllowRo)]),
        ..Default::default()
    })
    .expect("session setup");

    let result = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(s.mnt_path("hello.txt"));
    assert!(result.is_err(), "O_RDWR should be denied with allow-ro");
}

// ── allow-rw permits truncate/append ──

/// allow-rw should permit O_TRUNC.
#[test]
fn truncate_allowed_on_allow_rw() {
    let s = AgfsSession::new_with_config(Config {
        ask_default: Some(Perm::Deny),
        rules: BTreeMap::from([("/".into(), Perm::AllowRw)]),
        ..Default::default()
    })
    .expect("session setup");

    fs::OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(s.mnt_path("hello.txt"))
        .expect("O_TRUNC should succeed with allow-rw");
}

// ── Newly created files respect permissions ──

/// A file created inside the sandbox should still be subject to perm gating
/// when reopened. The perm_gen fix ensures new inodes get re-resolved.
#[test]
fn newly_created_file_checked_on_reopen() {
    let s = AgfsSession::new_with_config(Config {
        ask_default: Some(Perm::Deny),
        rules: BTreeMap::from([("/".into(), Perm::AllowRw)]),
        ..Default::default()
    })
    .expect("session setup");

    // Create a file (dir op, bypasses perm).
    fs::write(s.mnt_path("newfile.txt"), "hello").expect("create should succeed");

    // Now change rules to deny and re-read.
    s.cli(&["unmount", "--force"]).unwrap();
    Config {
        ask_default: Some(Perm::Deny),
        rules: BTreeMap::from([("/".into(), Perm::Deny)]),
        ..Default::default()
    }
    .save(&s.root.join("agfs.toml"))
    .unwrap();
    std::process::Command::new(AGFS_BIN)
        .arg("mount")
        .current_dir(&s.root)
        .env("NO_COLOR", "1")
        .output()
        .expect("remount");

    let result = fs::read_to_string(s.mnt_path("hello.txt"));
    assert!(
        result.is_err(),
        "read should be denied after rule change to deny"
    );
}

// ── allow-rx ──

/// allow-rx should permit read + exec but deny O_TRUNC.
#[test]
fn allow_rx_denies_truncate() {
    let s = AgfsSession::new_with_config(Config {
        ask_default: Some(Perm::Deny),
        rules: BTreeMap::from([("/".into(), Perm::AllowRx)]),
        ..Default::default()
    })
    .expect("session setup");

    let result = fs::OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(s.mnt_path("hello.txt"));
    assert!(result.is_err(), "O_TRUNC should be denied with allow-rx");
}

/// allow-rx should deny O_APPEND.
#[test]
fn allow_rx_denies_append() {
    let s = AgfsSession::new_with_config(Config {
        ask_default: Some(Perm::Deny),
        rules: BTreeMap::from([("/".into(), Perm::AllowRx)]),
        ..Default::default()
    })
    .expect("session setup");

    let result = fs::OpenOptions::new()
        .append(true)
        .open(s.mnt_path("hello.txt"));
    assert!(result.is_err(), "O_APPEND should be denied with allow-rx");
}

// ── rmdir bypasses perm ──

/// rmdir should succeed even under deny because it is a directory inode op.
#[test]
fn rmdir_allowed_under_deny() {
    let s = AgfsSession::new_with_config(Config {
        ask_default: Some(Perm::Deny),
        rules: BTreeMap::from([("/".into(), Perm::Deny)]),
        ..Default::default()
    })
    .expect("session setup");

    fs::remove_dir(s.mnt_path("subdir")).expect("rmdir should succeed: dir ops bypass agfs perm");
}

/// rename should succeed under deny because it is a directory inode op.
#[test]
fn rename_allowed_under_deny() {
    let s = AgfsSession::new_with_config(Config {
        ask_default: Some(Perm::Deny),
        rules: BTreeMap::from([("/".into(), Perm::Deny)]),
        ..Default::default()
    })
    .expect("session setup");

    fs::rename(s.mnt_path("hello.txt"), s.mnt_path("renamed.txt"))
        .expect("rename should succeed: dir ops bypass agfs perm");
}

/// file creation should succeed under deny because it is a directory inode op.
#[test]
fn create_allowed_under_deny() {
    let s = AgfsSession::new_with_config(Config {
        ask_default: Some(Perm::Deny),
        rules: BTreeMap::from([("/".into(), Perm::Deny)]),
        ..Default::default()
    })
    .expect("session setup");

    // O_CREAT goes through agfs_create (dir op) then agfs_open checks perm.
    // fs::write uses O_WRONLY|O_CREAT|O_TRUNC, so the create (dir op) succeeds
    // but the open (file op) should fail under deny.
    let result = fs::write(s.mnt_path("newfile.txt"), "data");
    assert!(
        result.is_err(),
        "write to new file should fail under deny (open is gated)"
    );

    // The file was created in staging (dir op succeeded). Verify via status.
    let status = s.cli(&["status"]).unwrap();
    assert!(
        status.contains("newfile.txt"),
        "status should show the created file: {status}"
    );
}

// ── readdir is not gated ──

/// Listing a directory's contents should work even under deny
/// (readdir is a directory operation, not a regular file open).
#[test]
fn readdir_allowed_under_deny() {
    let s = AgfsSession::new_with_config(Config {
        ask_default: Some(Perm::Deny),
        rules: BTreeMap::from([("/".into(), Perm::Deny)]),
        ..Default::default()
    })
    .expect("session setup");

    let entries: Vec<_> = fs::read_dir(s.mnt_path(""))
        .expect("readdir should succeed under deny")
        .collect();
    assert!(!entries.is_empty(), "directory should have entries");
}

// ── ask_timeout applies default when no daemon ──

/// With ask_timeout set and no daemon, the ask should time out and
/// apply the ask_default.
#[test]
fn ask_timeout_applies_default() {
    let s = AgfsSession::new_with_config(Config {
        ask_timeout: Some(1),
        ask_default: Some(Perm::Deny),
        rules: BTreeMap::new(),
        ..Default::default()
    })
    .expect("session setup");

    // No daemon running — ask times out, applies ask_default=deny.
    let result = fs::read_to_string(s.mnt_path("hello.txt"));
    assert!(
        result.is_err(),
        "read should be denied when ask times out with ask_default=deny"
    );
}

/// With ask_timeout and ask_default=allow, timed out ask should allow.
#[test]
fn ask_timeout_applies_allow_default() {
    let s = AgfsSession::new_with_config(Config {
        ask_timeout: Some(1),
        ask_default: Some(Perm::Allow),
        rules: BTreeMap::new(),
        ..Default::default()
    })
    .expect("session setup");

    let content = fs::read_to_string(s.mnt_path("hello.txt"))
        .expect("read should succeed when ask times out with ask_default=allow");
    assert_eq!(content, "base content\n");
}

// ── Deep nested rule inheritance ──

/// Rules resolve to the closest ancestor: / = deny, root/a/b = allow-rw.
/// Files under a/b/c inherit allow-rw; files at top level get denied.
#[test]
fn deep_nested_rules_closest_wins() {
    let s = AgfsSession::new_with_config(Config {
        permission: false,
        rules: BTreeMap::new(),
        ..Default::default()
    })
    .expect("session setup");

    // Create base files directly (not through mount) so they survive remount.
    fs::create_dir_all(s.root.join("a/b/c")).expect("mkdir -p");
    fs::write(s.root.join("a/b/c/deep.txt"), "deep content\n").expect("create deep file");

    // Remount with tiered rules.
    s.cli(&["unmount"]).unwrap();
    Config {
        ask_default: Some(Perm::Deny),
        rules: BTreeMap::from([
            ("/".into(), Perm::Deny),
            (s.root.join("a/b").display().to_string(), Perm::AllowRw),
        ]),
        ..Default::default()
    }
    .save(&s.root.join("agfs.toml"))
    .unwrap();
    std::process::Command::new(AGFS_BIN)
        .arg("mount")
        .current_dir(&s.root)
        .env("NO_COLOR", "1")
        .output()
        .expect("remount");

    let content = fs::read_to_string(s.mnt_path("a/b/c/deep.txt"))
        .expect("deep file should be readable via inherited allow-rw");
    assert_eq!(content, "deep content\n");

    let result = fs::read_to_string(s.mnt_path("hello.txt"));
    assert!(
        result.is_err(),
        "top-level file should be denied with / = deny"
    );
}

// ── Multiple different rules on different paths ──

/// Different directories can have different permission rules simultaneously.
#[test]
fn different_paths_different_rules() {
    let s = AgfsSession::new_with_config(Config {
        permission: false,
        rules: BTreeMap::new(),
        ..Default::default()
    })
    .expect("session setup");

    // Create base files directly.
    fs::create_dir_all(s.root.join("readonly")).expect("mkdir readonly");
    fs::write(s.root.join("readonly/data.txt"), "ro content\n").expect("create ro file");
    fs::create_dir_all(s.root.join("writable")).expect("mkdir writable");
    fs::write(s.root.join("writable/data.txt"), "rw content\n").expect("create rw file");

    s.cli(&["unmount"]).unwrap();
    Config {
        ask_default: Some(Perm::Deny),
        rules: BTreeMap::from([
            (s.root.join("readonly").display().to_string(), Perm::AllowRo),
            (s.root.join("writable").display().to_string(), Perm::AllowRw),
        ]),
        ..Default::default()
    }
    .save(&s.root.join("agfs.toml"))
    .unwrap();
    std::process::Command::new(AGFS_BIN)
        .arg("mount")
        .current_dir(&s.root)
        .env("NO_COLOR", "1")
        .output()
        .expect("remount");

    // Read should work in both.
    fs::read_to_string(s.mnt_path("readonly/data.txt"))
        .expect("read should succeed in allow-ro dir");
    fs::read_to_string(s.mnt_path("writable/data.txt"))
        .expect("read should succeed in allow-rw dir");

    // Write should fail in readonly, succeed in writable.
    let result = fs::write(s.mnt_path("readonly/data.txt"), "modified\n");
    assert!(result.is_err(), "write should fail in allow-ro dir");
    fs::write(s.mnt_path("writable/data.txt"), "modified\n")
        .expect("write should succeed in allow-rw dir");
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

    let result = fs::read_to_string(s.mnt_path("hello.txt"));
    assert!(result.is_err(), "read should fail under deny");

    // `rule add` takes a host path and resolves it through the mount internally.
    s.cli(&["rule", "add", &s.root.display().to_string(), "allow-rw"])
        .unwrap();

    let content = fs::read_to_string(s.mnt_path("hello.txt"))
        .expect("read should succeed after live rule add");
    assert_eq!(content, "base content\n");
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

    s.cli(&["rule", "add", &s.root.display().to_string(), "allow-rw"])
        .unwrap();
    fs::read_to_string(s.mnt_path("hello.txt")).expect("read should succeed with allow-rw rule");

    s.cli(&["rule", "remove", &s.root.display().to_string()])
        .unwrap();
    let result = fs::read_to_string(s.mnt_path("hello.txt"));
    assert!(result.is_err(), "read should fail after rule removal");
}

// ── Rename across permission boundaries ──

/// Renaming a file from an allowed dir to a denied dir should succeed (dir op),
/// but reading the renamed file in the denied dir should fail after a cache
/// invalidation (the inode may still cache the old permission until perm_gen
/// is bumped by a rule change).
#[test]
fn rename_across_permission_boundary() {
    let s = AgfsSession::new_with_config(Config {
        permission: false,
        rules: BTreeMap::new(),
        ..Default::default()
    })
    .expect("session setup");

    // Create base files directly.
    fs::create_dir_all(s.root.join("allowed")).expect("mkdir allowed");
    fs::create_dir_all(s.root.join("denied")).expect("mkdir denied");
    fs::write(s.root.join("allowed/file.txt"), "content\n").expect("create file");

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
    std::process::Command::new(AGFS_BIN)
        .arg("mount")
        .current_dir(&s.root)
        .env("NO_COLOR", "1")
        .output()
        .expect("remount");

    // Can read the file in the allowed dir.
    fs::read_to_string(s.mnt_path("allowed/file.txt"))
        .expect("reading file in allowed dir should succeed");

    // Rename is a dir op — should succeed.
    fs::rename(
        s.mnt_path("allowed/file.txt"),
        s.mnt_path("denied/file.txt"),
    )
    .expect("rename is a dir op and should succeed");

    // Force cache invalidation so the permission is re-resolved at the new location.
    s.cli(&[
        "rule",
        "add",
        &s.root.join("denied").display().to_string(),
        "deny",
    ])
    .unwrap();

    // Reading from the denied directory should now fail.
    let result = fs::read_to_string(s.mnt_path("denied/file.txt"));
    assert!(
        result.is_err(),
        "reading renamed file in denied dir should fail"
    );
}

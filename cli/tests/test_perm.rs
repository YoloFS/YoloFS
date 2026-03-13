use crate::helpers::AgfsSession;
use std::fs;

/// With no daemon and ask_default=deny, reading an unruled file should fail.
#[test]
fn no_daemon_denies_by_default() {
    let config = format!(
        "[mount]\nask_default = \"deny\"\n\n[rules]\n"
    );
    let s = AgfsSession::new_with_config(&config).expect("session setup");

    let result = fs::read_to_string(s.mnt_path("hello.txt"));
    assert!(result.is_err(), "read should be denied without daemon or rule");
}

/// With no daemon and ask_default=allow, reading an unruled file should succeed.
#[test]
fn no_daemon_allows_when_configured() {
    let config = format!(
        "[mount]\nask_default = \"allow\"\n\n[rules]\n"
    );
    let s = AgfsSession::new_with_config(&config).expect("session setup");

    let content = fs::read_to_string(s.mnt_path("hello.txt"))
        .expect("read should succeed with ask_default=allow");
    assert_eq!(content, "base content\n");
}

/// An explicit allow rule should bypass the ask mechanism entirely.
#[test]
fn explicit_rule_bypasses_ask() {
    let s = AgfsSession::new_with_config(&format!(
        "[mount]\nask_default = \"deny\"\n\n[rules]\n\"{}\" = \"allow\"\n",
        // Rule the session root so hello.txt is covered
        "/"
    )).expect("session setup");

    let content = fs::read_to_string(s.mnt_path("hello.txt"))
        .expect("read should succeed with explicit allow rule");
    assert_eq!(content, "base content\n");
}

/// An explicit deny rule should block access even with noperm=false.
#[test]
fn deny_rule_blocks_access() {
    let s = AgfsSession::new_with_config(&format!(
        "[mount]\nask_default = \"allow\"\n\n[rules]\n\"{}\" = \"deny\"\n",
        "/"
    )).expect("session setup");

    let result = fs::read_to_string(s.mnt_path("hello.txt"));
    assert!(result.is_err(), "read should be denied with explicit deny rule");
}

/// With noperm=true, everything is allowed regardless of rules.
#[test]
fn noperm_allows_everything() {
    let s = AgfsSession::new_with_config(
        "[mount]\nnoperm = true\n\n[rules]\n\"/\" = \"deny\"\n"
    ).expect("session setup");

    let content = fs::read_to_string(s.mnt_path("hello.txt"))
        .expect("read should succeed with noperm=true even with deny rule");
    assert_eq!(content, "base content\n");
}

/// allow-ro should permit reads but deny writes.
#[test]
fn allow_ro_permits_read_denies_write() {
    let s = AgfsSession::new_with_config(&format!(
        "[mount]\nask_default = \"deny\"\n\n[rules]\n\"{}\" = \"allow-ro\"\n",
        "/"
    )).expect("session setup");

    // Read should succeed
    let content = fs::read_to_string(s.mnt_path("hello.txt"))
        .expect("read should succeed with allow-ro");
    assert_eq!(content, "base content\n");

    // Write should fail
    let result = fs::write(s.mnt_path("hello.txt"), "modified\n");
    assert!(result.is_err(), "write should be denied with allow-ro rule");
}

// ── allow-rw tests (perm.c: agfs_check_perm, inode.c: agfs_permission) ──

/// allow-rw should permit reads.
#[test]
fn allow_rw_permits_read() {
    let s = AgfsSession::new_with_config(&format!(
        "[mount]\nask_default = \"deny\"\n\n[rules]\n\"{}\" = \"allow-rw\"\n",
        "/"
    )).expect("session setup");

    let content = fs::read_to_string(s.mnt_path("hello.txt"))
        .expect("read should succeed with allow-rw");
    assert_eq!(content, "base content\n");
}

/// allow-rw should permit writes.
#[test]
fn allow_rw_permits_write() {
    let s = AgfsSession::new_with_config(&format!(
        "[mount]\nask_default = \"deny\"\n\n[rules]\n\"{}\" = \"allow-rw\"\n",
        "/"
    )).expect("session setup");

    fs::write(s.mnt_path("hello.txt"), "modified\n")
        .expect("write should succeed with allow-rw");
}

/// allow-rw should deny exec (MAY_EXEC check in agfs_permission).
#[test]
fn allow_rw_denies_exec() {
    let s = AgfsSession::new_with_config(&format!(
        "[mount]\nask_default = \"deny\"\n\n[rules]\n\"{}\" = \"allow-rw\"\n",
        "/"
    )).expect("session setup");

    let result = std::process::Command::new(s.mnt_path("test.sh"))
        .output();
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
    let s = AgfsSession::new_with_config(&format!(
        "[mount]\nask_default = \"deny\"\n\n[rules]\n\"{}\" = \"allow-rx\"\n",
        "/"
    )).expect("session setup");

    let content = fs::read_to_string(s.mnt_path("hello.txt"))
        .expect("read should succeed with allow-rx");
    assert_eq!(content, "base content\n");
}

/// allow-rx should deny writes.
#[test]
fn allow_rx_denies_write() {
    let s = AgfsSession::new_with_config(&format!(
        "[mount]\nask_default = \"deny\"\n\n[rules]\n\"{}\" = \"allow-rx\"\n",
        "/"
    )).expect("session setup");

    let result = fs::write(s.mnt_path("hello.txt"), "modified\n");
    assert!(result.is_err(), "write should be denied with allow-rx rule");
}

/// allow-rx should permit exec.
#[test]
fn allow_rx_permits_exec() {
    let s = AgfsSession::new_with_config(&format!(
        "[mount]\nask_default = \"deny\"\n\n[rules]\n\"{}\" = \"allow-rx\"\n",
        "/"
    )).expect("session setup");

    let output = std::process::Command::new(s.mnt_path("test.sh"))
        .output()
        .expect("should be able to spawn executable");
    assert!(output.status.success(), "exec should succeed with allow-rx");
}

// ── allow (full access) ──

/// allow should permit exec.
#[test]
fn allow_permits_exec() {
    let s = AgfsSession::new_with_config(&format!(
        "[mount]\nask_default = \"deny\"\n\n[rules]\n\"{}\" = \"allow\"\n",
        "/"
    )).expect("session setup");

    let output = std::process::Command::new(s.mnt_path("test.sh"))
        .output()
        .expect("should be able to spawn executable");
    assert!(output.status.success(), "exec should succeed with allow");
}

// ── deny (all blocked) ──

/// deny should block writes (not just reads).
#[test]
fn deny_blocks_write() {
    let s = AgfsSession::new_with_config(&format!(
        "[mount]\nask_default = \"allow\"\n\n[rules]\n\"{}\" = \"deny\"\n",
        "/"
    )).expect("session setup");

    let result = fs::write(s.mnt_path("hello.txt"), "modified\n");
    assert!(result.is_err(), "write should be denied with deny rule");
}

/// deny should block exec.
#[test]
fn deny_blocks_exec() {
    let s = AgfsSession::new_with_config(&format!(
        "[mount]\nask_default = \"allow\"\n\n[rules]\n\"{}\" = \"deny\"\n",
        "/"
    )).expect("session setup");

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
    let s = AgfsSession::new_with_config(
        "[mount]\nnoperm = true\n\n[rules]\n"
    ).expect("session setup");

    // Unmount, write config with root-path-dependent rules, remount
    s.cli(&["unmount"]).unwrap();
    let root_path = s.root.display().to_string();
    fs::write(
        s.root.join("agfs.toml"),
        format!(
            "[mount]\nask_default = \"deny\"\n\n[rules]\n\"/\" = \"deny\"\n\"{}\" = \"allow-rw\"\n",
            root_path,
        ),
    ).unwrap();
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
    let s = AgfsSession::new_with_config(
        "[mount]\nask_default = \"allow-ro\"\n\n[rules]\n"
    ).expect("session setup");

    let content = fs::read_to_string(s.mnt_path("hello.txt"))
        .expect("read should succeed with ask_default=allow-ro");
    assert_eq!(content, "base content\n");

    let result = fs::write(s.mnt_path("hello.txt"), "modified\n");
    assert!(result.is_err(), "write should be denied with ask_default=allow-ro");
}

/// ask_default=allow-rw: read OK, write OK.
#[test]
fn ask_default_allow_rw() {
    let s = AgfsSession::new_with_config(
        "[mount]\nask_default = \"allow-rw\"\n\n[rules]\n"
    ).expect("session setup");

    let content = fs::read_to_string(s.mnt_path("hello.txt"))
        .expect("read should succeed with ask_default=allow-rw");
    assert_eq!(content, "base content\n");

    fs::write(s.mnt_path("hello.txt"), "modified\n")
        .expect("write should succeed with ask_default=allow-rw");
}

/// ask_default=allow-rx: read OK, write denied.
#[test]
fn ask_default_allow_rx() {
    let s = AgfsSession::new_with_config(
        "[mount]\nask_default = \"allow-rx\"\n\n[rules]\n"
    ).expect("session setup");

    let content = fs::read_to_string(s.mnt_path("hello.txt"))
        .expect("read should succeed with ask_default=allow-rx");
    assert_eq!(content, "base content\n");

    let result = fs::write(s.mnt_path("hello.txt"), "modified\n");
    assert!(result.is_err(), "write should be denied with ask_default=allow-rx");
}

// ── Directory ops bypass agfs permission (inode.c: agfs_permission
//    delegates to lower FS for non-regular files) ──

/// mkdir should succeed even under a deny rule because directory ops
/// are checked against the lower FS, not agfs permission rules.
#[test]
fn mkdir_allowed_under_deny() {
    let s = AgfsSession::new_with_config(&format!(
        "[mount]\nask_default = \"deny\"\n\n[rules]\n\"{}\" = \"deny\"\n",
        "/"
    )).expect("session setup");

    fs::create_dir(s.mnt_path("newdir"))
        .expect("mkdir should succeed: dir ops bypass agfs perm");
}

/// unlink should succeed under allow-ro because inode removal is a
/// directory operation and doesn't go through agfs_permission on the file.
#[test]
fn unlink_allowed_under_allow_ro() {
    let s = AgfsSession::new_with_config(&format!(
        "[mount]\nask_default = \"deny\"\n\n[rules]\n\"{}\" = \"allow-ro\"\n",
        "/"
    )).expect("session setup");

    // unlink goes through agfs_unlink which creates a whiteout
    fs::remove_file(s.mnt_path("hello.txt"))
        .expect("unlink should succeed: it is a dir op on the parent");
}

/// symlink creation should succeed under deny because symlink is a
/// directory inode operation.
#[test]
fn symlink_allowed_under_deny() {
    let s = AgfsSession::new_with_config(&format!(
        "[mount]\nask_default = \"deny\"\n\n[rules]\n\"{}\" = \"deny\"\n",
        "/"
    )).expect("session setup");

    std::os::unix::fs::symlink("hello.txt", s.mnt_path("link.txt"))
        .expect("symlink should succeed: dir ops bypass agfs perm");
}


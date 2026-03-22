use crate::helpers::AgfsSession;
use agfs::config::{Config, Perm};
use std::collections::BTreeMap;
use std::fs;

// ── allow-ro tests (perm.c: agfs_check_perm, inode.c: agfs_permission) ──

/// allow-ro should permit reads but deny writes.
#[test]
fn allow_ro_permits_read_denies_write() {
    let s = AgfsSession::new_with_config(Config {
        ask_default: Some(Perm::Deny),
        rules: BTreeMap::from([("/".into(), Perm::AllowRo)]),
        ..Default::default()
    })
    .expect("session setup");

    s.run_in_namespace(|| {
        // Read should succeed
        let content =
            fs::read_to_string(s.mnt_path("hello.txt")).expect("read should succeed with allow-ro");
        assert_eq!(content, "base content\n");

        // Write should fail
        let result = fs::write(s.mnt_path("hello.txt"), "modified\n");
        assert!(result.is_err(), "write should be denied with allow-ro rule");
    });
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

    s.run_in_namespace(|| {
        let content =
            fs::read_to_string(s.mnt_path("hello.txt")).expect("read should succeed with allow-rw");
        assert_eq!(content, "base content\n");
    });
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

    s.run_in_namespace(|| {
        fs::write(s.mnt_path("hello.txt"), "modified\n").expect("write should succeed with allow-rw");
    });
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

    s.run_in_namespace(|| {
        let result = std::process::Command::new(s.mnt_path("test.sh")).output();
        // execve should fail with EACCES (permission denied)
        assert!(
            result.is_err() || !result.unwrap().status.success(),
            "exec should be denied with allow-rw"
        );
    });
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

    s.run_in_namespace(|| {
        let content =
            fs::read_to_string(s.mnt_path("hello.txt")).expect("read should succeed with allow-rx");
        assert_eq!(content, "base content\n");
    });
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

    s.run_in_namespace(|| {
        let result = fs::write(s.mnt_path("hello.txt"), "modified\n");
        assert!(result.is_err(), "write should be denied with allow-rx rule");
    });
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

    s.run_in_namespace(|| {
        let output = std::process::Command::new(s.mnt_path("test.sh"))
            .output()
            .expect("should be able to spawn executable");
        assert!(output.status.success(), "exec should succeed with allow-rx");
    });
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

    s.run_in_namespace(|| {
        let output = std::process::Command::new(s.mnt_path("test.sh"))
            .output()
            .expect("should be able to spawn executable");
        assert!(output.status.success(), "exec should succeed with allow");
    });
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

    s.run_in_namespace(|| {
        let result = fs::write(s.mnt_path("hello.txt"), "modified\n");
        assert!(result.is_err(), "write should be denied with deny rule");
    });
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

    s.run_in_namespace(|| {
        let result = std::process::Command::new(s.mnt_path("test.sh")).output();
        assert!(
            result.is_err() || !result.unwrap().status.success(),
            "exec should be denied with deny rule"
        );
    });
}

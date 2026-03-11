// agfs CLI — ioctl.rs
//
// Binary protocol helpers for communicating with the kernel module
// via ioctl on .agfs/mnt directory.

use anyhow::{Context, Result};
use std::fs::{File, OpenOptions};

use std::os::unix::io::AsRawFd;
use std::path::Path;

pub const AGFS_PATH_MAX: usize = 256;

// Operation types
pub const AGFS_OP_READ: u32 = 1;
pub const AGFS_OP_WRITE: u32 = 2;
pub const AGFS_OP_EXEC: u32 = 3;

// Permission values
pub const AGFS_PERM_NONE: u8 = 0;
pub const AGFS_PERM_ASK: u8 = 1;
pub const AGFS_PERM_ALLOW: u8 = 2;
pub const AGFS_PERM_ALLOW_RW: u8 = 3;
pub const AGFS_PERM_ALLOW_RO: u8 = 4;
pub const AGFS_PERM_ALLOW_RX: u8 = 5;
pub const AGFS_PERM_DENY: u8 = 6;

// Log event types
pub const AGFS_LOG_OPEN: u8 = 1;
pub const AGFS_LOG_ASK: u8 = 2;
pub const AGFS_LOG_DECISION: u8 = 3;
pub const AGFS_LOG_DENY: u8 = 4;
pub const AGFS_LOG_COW: u8 = 5;
pub const AGFS_LOG_RULE: u8 = 6;
pub const AGFS_LOG_COMMIT: u8 = 7;
pub const AGFS_LOG_ABORT: u8 = 8;

// Ioctl command numbers — must match kmod/agfs.h
// _IOW('A', 10, struct agfs_ioc_rule)  -> direction=1, type='A'=0x41, nr=10, size=264
// _IOW('A', 11, struct agfs_ioc_rule)
// _IO('A', 20)
nix::ioctl_write_ptr!(ioctl_rule_add, b'A', 10, AgfsIocRule);
nix::ioctl_write_ptr!(ioctl_rule_remove, b'A', 11, AgfsIocRule);
nix::ioctl_none!(ioctl_cache_inval, b'A', 20);
nix::ioctl_read!(ioctl_ctl_read, b'A', 30, AgfsCtlRequest);
nix::ioctl_write_ptr!(ioctl_ctl_write, b'A', 31, AgfsCtlResponse);

/// Matches `struct agfs_ioc_rule` in the kernel.
#[repr(C)]
pub struct AgfsIocRule {
    pub path: [u8; AGFS_PATH_MAX],
    pub perm: u8,
    pub _pad: [u8; 7],
}

/// Matches `struct agfs_ctl_request` in the kernel (kernel → userspace).
#[repr(C)]
#[derive(Clone)]
pub struct AgfsCtlRequest {
    pub id: u64,
    pub op: u32,
    pub pid: u32,
    pub comm: [u8; 16],
    pub path: [u8; AGFS_PATH_MAX],
}

/// Matches `struct agfs_ctl_response` in the kernel (userspace → kernel).
#[repr(C)]
pub struct AgfsCtlResponse {
    pub id: u64,
    pub decision: u8,
    pub _pad: [u8; 7],
}

/// Matches `struct agfs_log_entry` in the kernel.
#[repr(C)]
#[derive(Clone)]
pub struct AgfsLogEntry {
    pub timestamp_ns: u64,
    pub req_id: u64,
    pub op: u32,
    pub pid: u32,
    pub event: u8,
    pub perm: u8,
    pub _pad: u16,
    pub comm: [u8; 16],
    pub path: [u8; AGFS_PATH_MAX],
}

impl AgfsIocRule {
    pub fn new(path: &str, perm: u8) -> Self {
        let mut rule = Self {
            path: [0u8; AGFS_PATH_MAX],
            perm,
            _pad: [0u8; 7],
        };
        let bytes = path.as_bytes();
        let len = bytes.len().min(AGFS_PATH_MAX - 1);
        rule.path[..len].copy_from_slice(&bytes[..len]);
        rule
    }
}

impl AgfsCtlRequest {
    pub fn path_str(&self) -> &str {
        let end = self.path.iter().position(|&b| b == 0).unwrap_or(AGFS_PATH_MAX);
        std::str::from_utf8(&self.path[..end]).unwrap_or("<invalid>")
    }

    pub fn comm_str(&self) -> &str {
        let end = self.comm.iter().position(|&b| b == 0).unwrap_or(16);
        std::str::from_utf8(&self.comm[..end]).unwrap_or("<invalid>")
    }

    pub fn op_str(&self) -> &'static str {
        match self.op {
            AGFS_OP_READ => "read",
            AGFS_OP_WRITE => "write",
            AGFS_OP_EXEC => "exec",
            _ => "unknown",
        }
    }
}

impl AgfsLogEntry {
    pub fn path_str(&self) -> &str {
        let end = self.path.iter().position(|&b| b == 0).unwrap_or(AGFS_PATH_MAX);
        std::str::from_utf8(&self.path[..end]).unwrap_or("<invalid>")
    }

    pub fn comm_str(&self) -> &str {
        let end = self.comm.iter().position(|&b| b == 0).unwrap_or(16);
        std::str::from_utf8(&self.comm[..end]).unwrap_or("<invalid>")
    }

    pub fn event_str(&self) -> &'static str {
        match self.event {
            AGFS_LOG_OPEN => "OPEN",
            AGFS_LOG_ASK => "ASK",
            AGFS_LOG_DECISION => "DECISION",
            AGFS_LOG_DENY => "DENY",
            AGFS_LOG_COW => "COW",
            AGFS_LOG_RULE => "RULE",
            AGFS_LOG_COMMIT => "COMMIT",
            AGFS_LOG_ABORT => "ABORT",
            _ => "UNKNOWN",
        }
    }

    pub fn perm_str(&self) -> &'static str {
        perm_to_str(self.perm)
    }
}

pub fn perm_to_str(perm: u8) -> &'static str {
    match perm {
        AGFS_PERM_NONE => "none",
        AGFS_PERM_ASK => "ask",
        AGFS_PERM_ALLOW => "allow",
        AGFS_PERM_ALLOW_RW => "allow-rw",
        AGFS_PERM_ALLOW_RO => "allow-ro",
        AGFS_PERM_ALLOW_RX => "allow-rx",
        AGFS_PERM_DENY => "deny",
        _ => "unknown",
    }
}

pub fn perm_from_str(s: &str) -> Option<u8> {
    match s {
        "ask" => Some(AGFS_PERM_ASK),
        "allow" => Some(AGFS_PERM_ALLOW),
        "allow-rw" => Some(AGFS_PERM_ALLOW_RW),
        "allow-ro" => Some(AGFS_PERM_ALLOW_RO),
        "allow-rx" => Some(AGFS_PERM_ALLOW_RX),
        "deny" => Some(AGFS_PERM_DENY),
        _ => None,
    }
}

/// Read one `AgfsCtlRequest` via ioctl on a directory fd.
pub fn read_request(fd: &File) -> Result<AgfsCtlRequest> {
    let mut req = AgfsCtlRequest {
        id: 0,
        op: 0,
        pid: 0,
        comm: [0u8; 16],
        path: [0u8; AGFS_PATH_MAX],
    };
    unsafe { ioctl_ctl_read(fd.as_raw_fd(), &mut req) }
        .context("ioctl CTL_READ")?;
    Ok(req)
}

/// Write one `AgfsCtlResponse` via ioctl on a directory fd.
pub fn write_response(fd: &File, id: u64, decision: u8) -> Result<()> {
    let resp = AgfsCtlResponse {
        id,
        decision,
        _pad: [0u8; 7],
    };
    unsafe { ioctl_ctl_write(fd.as_raw_fd(), &resp) }
        .context("ioctl CTL_WRITE")?;
    Ok(())
}

/// Open the mount point directory for ioctl.
pub fn open(agfs_dir: &Path) -> Result<File> {
    let mnt = agfs_dir.join("mnt");
    OpenOptions::new()
        .read(true)
        .open(&mnt)
        .context("opening .agfs/mnt for ioctl")
}

/// Send AGFS_IOC_RULE_ADD ioctl.
pub fn add_rule(fd: &File, path: &str, perm: u8) -> Result<()> {
    let rule = AgfsIocRule::new(path, perm);
    unsafe { ioctl_rule_add(fd.as_raw_fd(), &rule) }
        .with_context(|| format!("ioctl RULE_ADD for {path}"))?;
    Ok(())
}

/// Send AGFS_IOC_RULE_REMOVE ioctl.
pub fn remove_rule(fd: &File, path: &str) -> Result<()> {
    let rule = AgfsIocRule::new(path, AGFS_PERM_NONE);
    unsafe { ioctl_rule_remove(fd.as_raw_fd(), &rule) }
        .with_context(|| format!("ioctl RULE_REMOVE for {path}"))?;
    Ok(())
}

/// Send AGFS_IOC_CACHE_INVAL ioctl.
pub fn invalidate_cache(fd: &File) -> Result<()> {
    unsafe { ioctl_cache_inval(fd.as_raw_fd()) }
        .context("ioctl CACHE_INVAL")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn perm_roundtrip() {
        for (s, v) in [
            ("ask", AGFS_PERM_ASK),
            ("allow", AGFS_PERM_ALLOW),
            ("allow-rw", AGFS_PERM_ALLOW_RW),
            ("allow-ro", AGFS_PERM_ALLOW_RO),
            ("allow-rx", AGFS_PERM_ALLOW_RX),
            ("deny", AGFS_PERM_DENY),
        ] {
            assert_eq!(perm_from_str(s), Some(v));
            assert_eq!(perm_to_str(v), s);
        }
    }

    #[test]
    fn perm_from_str_unknown() {
        assert_eq!(perm_from_str("bogus"), None);
        assert_eq!(perm_from_str(""), None);
    }

    #[test]
    fn perm_to_str_unknown() {
        assert_eq!(perm_to_str(255), "unknown");
    }

    #[test]
    fn ioc_rule_new_basic() {
        let rule = AgfsIocRule::new("/foo/bar", AGFS_PERM_ALLOW);
        assert_eq!(rule.perm, AGFS_PERM_ALLOW);
        assert_eq!(&rule.path[..8], b"/foo/bar");
        assert_eq!(rule.path[8], 0);
    }

    #[test]
    fn ioc_rule_new_truncates_long_path() {
        let long = "a".repeat(300);
        let rule = AgfsIocRule::new(&long, AGFS_PERM_DENY);
        // Path is truncated to AGFS_PATH_MAX - 1 = 255 bytes
        assert_eq!(rule.path[AGFS_PATH_MAX - 1], 0);
        assert_eq!(rule.path[AGFS_PATH_MAX - 2], b'a');
    }

    #[test]
    fn ctl_request_path_str() {
        let mut req = AgfsCtlRequest {
            id: 1,
            op: AGFS_OP_READ,
            pid: 42,
            comm: [0u8; 16],
            path: [0u8; AGFS_PATH_MAX],
        };
        req.path[..5].copy_from_slice(b"hello");
        assert_eq!(req.path_str(), "hello");
    }

    #[test]
    fn ctl_request_op_str() {
        let mk = |op| AgfsCtlRequest {
            id: 0, op, pid: 0,
            comm: [0u8; 16],
            path: [0u8; AGFS_PATH_MAX],
        };
        assert_eq!(mk(AGFS_OP_READ).op_str(), "read");
        assert_eq!(mk(AGFS_OP_WRITE).op_str(), "write");
        assert_eq!(mk(AGFS_OP_EXEC).op_str(), "exec");
        assert_eq!(mk(99).op_str(), "unknown");
    }

    #[test]
    fn log_entry_event_str() {
        let mk = |event| AgfsLogEntry {
            timestamp_ns: 0, req_id: 0, op: 0, pid: 0,
            event, perm: 0, _pad: 0,
            comm: [0u8; 16],
            path: [0u8; AGFS_PATH_MAX],
        };
        assert_eq!(mk(AGFS_LOG_OPEN).event_str(), "OPEN");
        assert_eq!(mk(AGFS_LOG_COW).event_str(), "COW");
        assert_eq!(mk(AGFS_LOG_COMMIT).event_str(), "COMMIT");
        assert_eq!(mk(AGFS_LOG_ABORT).event_str(), "ABORT");
        assert_eq!(mk(255).event_str(), "UNKNOWN");
    }

    #[test]
    fn struct_sizes() {
        // Must match the kernel struct sizes for binary protocol compat
        assert_eq!(size_of::<AgfsCtlRequest>(), 288);
        assert_eq!(size_of::<AgfsCtlResponse>(), 16);
        assert_eq!(size_of::<AgfsLogEntry>(), 304);
        assert_eq!(size_of::<AgfsIocRule>(), 264);
    }
}

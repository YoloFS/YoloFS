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

// Ioctl command numbers — must match kmod/agfs.h
// _IOW('A', 10, struct agfs_ioc_rule)  -> direction=1, type='A'=0x41, nr=10, size=264
// _IOW('A', 11, struct agfs_ioc_rule)
// _IO('A', 20)
nix::ioctl_write_ptr!(ioctl_rule_add, b'A', 10, AgfsIocRule);
nix::ioctl_write_ptr!(ioctl_rule_remove, b'A', 11, AgfsIocRule);
nix::ioctl_none!(ioctl_cache_inval, b'A', 20);
nix::ioctl_read!(ioctl_get_request, b'A', 30, AgfsCtlRequest);
nix::ioctl_write_ptr!(ioctl_put_response, b'A', 31, AgfsCtlResponse);

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

/// Read one `AgfsCtlRequest` via ioctl on a directory fd.
pub fn read_request(fd: &File) -> std::result::Result<AgfsCtlRequest, nix::errno::Errno> {
    let mut req = AgfsCtlRequest {
        id: 0,
        op: 0,
        pid: 0,
        comm: [0u8; 16],
        path: [0u8; AGFS_PATH_MAX],
    };
    unsafe { ioctl_get_request(fd.as_raw_fd(), &mut req) }?;
    Ok(req)
}

/// Write one `AgfsCtlResponse` via ioctl on a directory fd.
pub fn write_response(fd: &File, id: u64, decision: u8) -> Result<()> {
    let resp = AgfsCtlResponse {
        id,
        decision,
        _pad: [0u8; 7],
    };
    unsafe { ioctl_put_response(fd.as_raw_fd(), &resp) }
        .context("ioctl PUT_RESPONSE")?;
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
    fn struct_sizes() {
        // Must match the kernel struct sizes for binary protocol compat
        assert_eq!(size_of::<AgfsCtlRequest>(), 288);
        assert_eq!(size_of::<AgfsCtlResponse>(), 16);
        assert_eq!(size_of::<AgfsIocRule>(), 264);
    }
}

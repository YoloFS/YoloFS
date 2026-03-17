// agfs CLI — ioctl.rs
//
// Binary protocol helpers for communicating with the kernel module
// via ioctl on .agfs/mnt directory.

use anyhow::{Context, Result};
use std::fs::{File, OpenOptions};

use std::os::unix::io::AsRawFd;
use std::path::Path;

/// Maximum path length (including NUL) — must match kmod/agfs.h.
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
nix::ioctl_write_ptr!(ioctl_rule_add, b'A', 10, AgfsIocRule);
nix::ioctl_write_ptr!(ioctl_rule_remove, b'A', 11, AgfsIocRule);
nix::ioctl_readwrite!(ioctl_get_request, b'A', 30, AgfsCtlRequest);
nix::ioctl_write_ptr!(ioctl_put_response, b'A', 31, AgfsCtlResponse);
nix::ioctl_readwrite!(ioctl_checkpoint, b'A', 40, AgfsIocCheckpoint);
nix::ioctl_write_ptr!(ioctl_restore, b'A', 41, AgfsIocRestore);

/// Matches `struct agfs_ioc_rule` in the kernel.
#[repr(C)]
pub struct AgfsIocRule {
    pub path_ptr: u64,
    pub path_len: u16,
    pub perm: u8,
    pub _pad: [u8; 5],
}

/// Matches `struct agfs_ctl_request` in the kernel (kernel → userspace).
/// Userspace provides path_ptr + path_buf_len; kernel fills the rest.
#[repr(C)]
#[derive(Clone)]
pub struct AgfsCtlRequest {
    pub id: u64,
    pub op: u32,
    pub pid: u32,
    pub comm: [u8; 16],
    pub path_ptr: u64,
    pub path_buf_len: u16,
    pub path_len: u16,
    pub _pad: [u8; 4],
}

/// Matches `struct agfs_ctl_response` in the kernel (userspace → kernel).
#[repr(C)]
pub struct AgfsCtlResponse {
    pub id: u64,
    pub decision: u8,
    pub _pad: [u8; 7],
}

/// Matches `struct agfs_ioc_checkpoint` in the kernel.
#[repr(C)]
pub struct AgfsIocCheckpoint {
    pub id: u64,
    pub name_ptr: u64,
    pub name_len: u16,
    pub _pad: [u8; 6],
}

/// Matches `struct agfs_ioc_restore_entry` in the kernel.
#[repr(C)]
pub struct AgfsIocRestoreEntry {
    pub path_ptr: u64,
    pub path_len: u16,
    pub d_type: u8,
    pub _pad1: [u8; 5],
    pub ino: u64,
    pub base_path_ptr: u64,
    pub base_path_len: u16,
    pub _pad2: [u8; 6],
}

/// Matches `struct agfs_ioc_restore` in the kernel.
#[repr(C)]
pub struct AgfsIocRestore {
    pub checkpoint_gen: u64,
    pub entry_count: u64,
    pub entries: *const AgfsIocRestoreEntry,
}

/// A dequeued permission request with owned path data.
pub struct PermRequest {
    pub id: u64,
    pub op: u32,
    pub pid: u32,
    pub comm: [u8; 16],
    pub path: String,
}

impl PermRequest {
    pub fn path_str(&self) -> &str {
        &self.path
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

/// Read one permission request via ioctl. Returns a `PermRequest` with
/// owned path data.
pub fn read_request(fd: &File) -> std::result::Result<PermRequest, nix::errno::Errno> {
    let mut path_buf = [0u8; AGFS_PATH_MAX];
    let mut req = AgfsCtlRequest {
        id: 0,
        op: 0,
        pid: 0,
        comm: [0u8; 16],
        path_ptr: path_buf.as_mut_ptr() as u64,
        path_buf_len: AGFS_PATH_MAX as u16,
        path_len: 0,
        _pad: [0u8; 4],
    };
    unsafe { ioctl_get_request(fd.as_raw_fd(), &mut req) }?;
    let path = std::str::from_utf8(&path_buf[..req.path_len as usize])
        .unwrap_or("<invalid>")
        .to_string();
    Ok(PermRequest {
        id: req.id,
        op: req.op,
        pid: req.pid,
        comm: req.comm,
        path,
    })
}

/// Write one `AgfsCtlResponse` via ioctl on a directory fd.
pub fn write_response(fd: &File, id: u64, decision: u8) -> Result<()> {
    let resp = AgfsCtlResponse {
        id,
        decision,
        _pad: [0u8; 7],
    };
    unsafe { ioctl_put_response(fd.as_raw_fd(), &resp) }.context("ioctl PUT_RESPONSE")?;
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

fn make_rule(path: &str, perm: u8) -> Result<AgfsIocRule> {
    let bytes = path.as_bytes();
    let path_len: u16 = bytes.len().try_into().context("path too long")?;
    Ok(AgfsIocRule {
        path_ptr: bytes.as_ptr() as u64,
        path_len,
        perm,
        _pad: [0u8; 5],
    })
}

/// Send AGFS_IOC_RULE_ADD ioctl.
pub fn add_rule(fd: &File, path: &str, perm: u8) -> Result<()> {
    let rule = make_rule(path, perm)?;
    unsafe { ioctl_rule_add(fd.as_raw_fd(), &rule) }
        .with_context(|| format!("ioctl RULE_ADD for {path}"))?;
    Ok(())
}

/// Send AGFS_IOC_RULE_REMOVE ioctl.
pub fn remove_rule(fd: &File, path: &str) -> Result<()> {
    let rule = make_rule(path, AGFS_PERM_NONE)?;
    unsafe { ioctl_rule_remove(fd.as_raw_fd(), &rule) }
        .with_context(|| format!("ioctl RULE_REMOVE for {path}"))?;
    Ok(())
}

/// Send AGFS_IOC_RESTORE ioctl. Resets staging state and optionally injects
/// dirent entries. For commit/abort, pass empty entries with checkpoint_gen=1.
pub fn restore(fd: &File, checkpoint_gen: u64, entries: &[AgfsIocRestoreEntry]) -> Result<()> {
    let hdr = AgfsIocRestore {
        checkpoint_gen,
        entry_count: entries.len() as u64,
        entries: if entries.is_empty() {
            std::ptr::null()
        } else {
            entries.as_ptr()
        },
    };
    unsafe { ioctl_restore(fd.as_raw_fd(), &hdr) }.context("ioctl RESTORE")?;
    Ok(())
}

/// Send AGFS_IOC_CHECKPOINT ioctl. Returns the assigned checkpoint ID.
pub fn create_checkpoint(fd: &File, name: &str) -> Result<u64> {
    let name_bytes = name.as_bytes();
    let name_len: u16 = name_bytes
        .len()
        .try_into()
        .context("checkpoint name too long")?;
    let mut chk = AgfsIocCheckpoint {
        id: 0,
        name_ptr: name_bytes.as_ptr() as u64,
        name_len,
        _pad: [0u8; 6],
    };
    unsafe { ioctl_checkpoint(fd.as_raw_fd(), &mut chk) }.context("ioctl CHECKPOINT")?;
    Ok(chk.id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn struct_sizes() {
        // Must match the kernel struct sizes for binary protocol compat
        assert_eq!(size_of::<AgfsCtlRequest>(), 48);
        assert_eq!(size_of::<AgfsCtlResponse>(), 16);
        assert_eq!(size_of::<AgfsIocRule>(), 16);
        assert_eq!(size_of::<AgfsIocCheckpoint>(), 24);
        assert_eq!(size_of::<AgfsIocRestoreEntry>(), 40);
        assert_eq!(size_of::<AgfsIocRestore>(), 24);
    }

    #[test]
    fn perm_request_helpers() {
        let req = PermRequest {
            id: 1,
            op: AGFS_OP_READ,
            pid: 42,
            comm: {
                let mut c = [0u8; 16];
                c[..4].copy_from_slice(b"bash");
                c
            },
            path: "hello".into(),
        };
        assert_eq!(req.path_str(), "hello");
        assert_eq!(req.comm_str(), "bash");
        assert_eq!(req.op_str(), "read");
    }

    #[test]
    fn op_str_all_variants() {
        let mk = |op| PermRequest {
            id: 0,
            op,
            pid: 0,
            comm: [0u8; 16],
            path: String::new(),
        };
        assert_eq!(mk(AGFS_OP_READ).op_str(), "read");
        assert_eq!(mk(AGFS_OP_WRITE).op_str(), "write");
        assert_eq!(mk(AGFS_OP_EXEC).op_str(), "exec");
        assert_eq!(mk(99).op_str(), "unknown");
    }

    #[test]
    fn make_rule_basic() {
        let rule = make_rule("/foo/bar", AGFS_PERM_ALLOW).unwrap();
        assert_eq!(rule.path_len, 8);
        assert_eq!(rule.perm, AGFS_PERM_ALLOW);
        assert_eq!(rule.path_ptr, "/foo/bar".as_ptr() as u64);
    }

    #[test]
    fn make_rule_rejects_oversized_path() {
        let long = "a".repeat(u16::MAX as usize + 1);
        assert!(make_rule(&long, AGFS_PERM_DENY).is_err());
    }
}

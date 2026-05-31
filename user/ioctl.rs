// yolo CLI — ioctl.rs
//
// Binary protocol helpers for communicating with the kernel module
// via ioctl on .yolofs/mnt/.ctl control file.

use anyhow::{Context, Result};
use std::fs::{File, OpenOptions};

use std::os::unix::io::AsRawFd;
use std::path::Path;

/// Maximum path length (including NUL) — must match kmod/yolofs.h.
pub const YOLO_PATH_MAX: usize = 256;

// Operation types
pub const YOLO_OP_READ: u32 = 1;
pub const YOLO_OP_WRITE: u32 = 2;
pub const YOLO_OP_EXEC: u32 = 3;

// Permission values
pub const YOLO_PERM_NONE: u8 = 0;
pub const YOLO_PERM_ASK: u8 = 1;
pub const YOLO_PERM_ALLOW: u8 = 2;
pub const YOLO_PERM_READ: u8 = 3;
pub const YOLO_PERM_DENY: u8 = 4;
pub const YOLO_PERM_HIDE: u8 = 5;

// Ioctl command numbers — must match kmod/yolofs.h
nix::ioctl_write_ptr!(ioctl_rule_add, b'A', 10, YoloIocRule);
nix::ioctl_write_ptr!(ioctl_rule_remove, b'A', 11, YoloIocRule);
nix::ioctl_readwrite!(ioctl_get_request, b'A', 30, YoloCtlRequest);
nix::ioctl_write_ptr!(ioctl_put_response, b'A', 31, YoloCtlResponse);
nix::ioctl_readwrite!(ioctl_mark, b'A', 40, YoloIocMark);
nix::ioctl_readwrite!(ioctl_jump, b'A', 41, YoloIocJump);

/// Matches `struct yolo_ioc_rule` in the kernel.
#[repr(C)]
pub struct YoloIocRule {
    pub path_ptr: u64,
    pub path_len: u16,
    pub perm: u8,
    pub _pad: [u8; 5],
}

/// Matches `struct yolo_ctl_request` in the kernel (kernel → userspace).
/// Userspace provides path_ptr + path_buf_len; kernel fills the rest.
#[repr(C)]
#[derive(Clone)]
pub struct YoloCtlRequest {
    pub id: u64,
    pub op: u32,
    pub pid: u32,
    pub comm: [u8; 16],
    pub path_ptr: u64,
    pub path_buf_len: u16,
    pub path_len: u16,
    pub _pad: [u8; 4],
}

/// Matches `struct yolo_ctl_response` in the kernel (userspace → kernel).
#[repr(C)]
pub struct YoloCtlResponse {
    pub id: u64,
    pub decision: u8,
    pub _pad: [u8; 7],
}

/// Mark ioctl flag: skip if no data records since last mark.
pub const YOLO_MARK_IF_CHANGED: u8 = 1;

/// Matches `struct yolo_ioc_mark` in the kernel.
#[repr(C)]
pub struct YoloIocMark {
    pub gen_id: u64,
    pub name_ptr: u64,
    pub name_len: u16,
    pub flags: u8,
    pub _pad: [u8; 5],
}

/// Matches `struct yolo_ioc_jump` in the kernel.
#[repr(C)]
pub struct YoloIocJump {
    pub target_gen: u64,
    pub new_gen: u64,
    pub tree_len: u64,
    pub tree_ptr: u64,
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
            YOLO_OP_READ => "read",
            YOLO_OP_WRITE => "write",
            YOLO_OP_EXEC => "exec",
            _ => "unknown",
        }
    }
}

/// Read one permission request via ioctl. Returns a `PermRequest` with
/// owned path data.
pub fn read_request(fd: &File) -> std::result::Result<PermRequest, nix::errno::Errno> {
    let mut path_buf = [0u8; YOLO_PATH_MAX];
    let mut req = YoloCtlRequest {
        id: 0,
        op: 0,
        pid: 0,
        comm: [0u8; 16],
        path_ptr: path_buf.as_mut_ptr() as u64,
        path_buf_len: YOLO_PATH_MAX as u16,
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

/// Write one `YoloCtlResponse` via ioctl on a directory fd.
pub fn write_response(fd: &File, id: u64, decision: u8) -> Result<()> {
    let resp = YoloCtlResponse {
        id,
        decision,
        _pad: [0u8; 7],
    };
    unsafe { ioctl_put_response(fd.as_raw_fd(), &resp) }.context("ioctl PUT_RESPONSE")?;
    Ok(())
}

/// Open the .ctl control file for ioctl operations.
pub fn open(yolo_dir: &Path) -> Result<File> {
    let ctl = yolo_dir.join("mnt").join(".ctl");
    OpenOptions::new()
        .read(true)
        .open(&ctl)
        .context("opening .yolofs/mnt/.ctl for ioctl")
}

fn make_rule(path: &str, perm: u8) -> Result<YoloIocRule> {
    let bytes = path.as_bytes();
    let path_len: u16 = bytes.len().try_into().context("path too long")?;
    Ok(YoloIocRule {
        path_ptr: bytes.as_ptr() as u64,
        path_len,
        perm,
        _pad: [0u8; 5],
    })
}

/// Send YOLO_IOC_RULE_ADD ioctl.
pub fn add_rule(fd: &File, path: &str, perm: u8) -> Result<()> {
    let rule = make_rule(path, perm)?;
    unsafe { ioctl_rule_add(fd.as_raw_fd(), &rule) }
        .with_context(|| format!("ioctl RULE_ADD for {path}"))?;
    Ok(())
}

/// Send YOLO_IOC_RULE_REMOVE ioctl.
pub fn remove_rule(fd: &File, path: &str) -> Result<()> {
    let rule = make_rule(path, YOLO_PERM_NONE)?;
    unsafe { ioctl_rule_remove(fd.as_raw_fd(), &rule) }
        .with_context(|| format!("ioctl RULE_REMOVE for {path}"))?;
    Ok(())
}

/// Send YOLO_IOC_JUMP ioctl. Resets staging state and optionally injects
/// a serialized DirTree. For commit/abort, pass an empty buffer with target_gen=0.
/// For jump, pass target_gen > 0; returns the new generation assigned.
pub fn jump(fd: &File, target_gen: u64, tree_buf: &[u8]) -> Result<u64> {
    let mut hdr = YoloIocJump {
        target_gen,
        new_gen: 0,
        tree_len: tree_buf.len() as u64,
        tree_ptr: if tree_buf.is_empty() {
            0
        } else {
            tree_buf.as_ptr() as u64
        },
    };
    unsafe { ioctl_jump(fd.as_raw_fd(), &mut hdr) }.context("ioctl JUMP")?;
    Ok(hdr.new_gen)
}

/// Send YOLO_IOC_MARK ioctl. Returns the assigned gen, or 0 if
/// skipped due to `YOLO_MARK_IF_CHANGED` with no pending changes.
pub fn mark(fd: &File, name: &str, flags: u8) -> Result<u64> {
    let name_bytes = name.as_bytes();
    let name_len: u16 = name_bytes.len().try_into().context("mark name too long")?;
    let mut mrk = YoloIocMark {
        gen_id: 0,
        name_ptr: name_bytes.as_ptr() as u64,
        name_len,
        flags,
        _pad: [0u8; 5],
    };
    unsafe { ioctl_mark(fd.as_raw_fd(), &mut mrk) }.context("ioctl MARK")?;
    Ok(mrk.gen_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn struct_sizes() {
        // Must match the kernel struct sizes for binary protocol compat
        assert_eq!(size_of::<YoloCtlRequest>(), 48);
        assert_eq!(size_of::<YoloCtlResponse>(), 16);
        assert_eq!(size_of::<YoloIocRule>(), 16);
        assert_eq!(size_of::<YoloIocMark>(), 24);
        assert_eq!(size_of::<YoloIocJump>(), 32);
    }

    #[test]
    fn perm_request_helpers() {
        let req = PermRequest {
            id: 1,
            op: YOLO_OP_READ,
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
        assert_eq!(mk(YOLO_OP_READ).op_str(), "read");
        assert_eq!(mk(YOLO_OP_WRITE).op_str(), "write");
        assert_eq!(mk(YOLO_OP_EXEC).op_str(), "exec");
        assert_eq!(mk(99).op_str(), "unknown");
    }

    #[test]
    fn make_rule_basic() {
        let rule = make_rule("/foo/bar", YOLO_PERM_ALLOW).unwrap();
        assert_eq!(rule.path_len, 8);
        assert_eq!(rule.perm, YOLO_PERM_ALLOW);
        assert_eq!(rule.path_ptr, "/foo/bar".as_ptr() as u64);
    }

    #[test]
    fn make_rule_rejects_oversized_path() {
        let long = "a".repeat(u16::MAX as usize + 1);
        assert!(make_rule(&long, YOLO_PERM_DENY).is_err());
    }
}

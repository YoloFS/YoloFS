// yolo CLI — ioctl.rs
//
// Binary protocol helpers for communicating with the kernel module
// via ioctl on a directory fd in the mount (the mount root, or "." from
// inside the mount). There is no separate control file.

use crate::perm::{Decision, Perm};
use anyhow::{Context, Result};
use std::fs::{File, OpenOptions};

use std::os::unix::io::AsRawFd;
use std::path::Path;

/// Maximum path length (including NUL) for the ask protocol's path buffers —
/// must match kmod/yolofs.h. Rule targets are not subject to it (they are
/// passed as O_PATH fds, not paths).
pub const YOLO_PATH_MAX: usize = 256;

// Operation types
pub const YOLO_OP_READ: u32 = 1;
pub const YOLO_OP_WRITE: u32 = 2;

// Permission values
pub const YOLO_PERM_UNSET: u8 = 0;
pub const YOLO_PERM_ASK: u8 = 1;
pub const YOLO_PERM_ALLOW: u8 = 2;
pub const YOLO_PERM_WRITE_ASK: u8 = 3;
pub const YOLO_PERM_READ_ONLY: u8 = 4;
pub const YOLO_PERM_DENY: u8 = 5;
pub const YOLO_PERM_HIDE: u8 = 6;

// Ask decision values
pub const YOLO_DECISION_DENY: u8 = 0;
pub const YOLO_DECISION_ALLOW: u8 = 1;

// Ioctl command numbers — must match kmod/yolofs.h
nix::ioctl_write_ptr!(ioctl_rule_set, b'A', 10, YoloIocRule);
nix::ioctl_readwrite!(ioctl_rule_resolve, b'A', 11, YoloIocRule);
nix::ioctl_read!(ioctl_ask_peek, b'A', 30, YoloIocAsk);
nix::ioctl_write_ptr!(ioctl_ask_decide, b'A', 31, YoloIocDecision);
nix::ioctl_readwrite!(ioctl_snapshot, b'A', 40, YoloIocSnapshot);
nix::ioctl_readwrite!(ioctl_travel, b'A', 41, YoloIocTravel);
nix::ioctl_write_ptr!(ioctl_restore, b'A', 42, YoloIocRestore);

/// Matches `struct yolo_ioc_rule` in the kernel. The rule target is an
/// O_PATH fd opened through the mount (see [`open_rule_target`]): the path
/// walk happens once, in our `open()`, and the kernel validates the exact
/// dentry the rule attaches to instead of re-resolving a string.
#[repr(C)]
pub struct YoloIocRule {
    pub fd: i32,
    pub perm: u8,
    pub _pad: [u8; 3],
}

/// Matches `struct yolo_ioc_ask` in the kernel (kernel → userspace).
#[repr(C)]
#[derive(Clone)]
pub struct YoloIocAsk {
    pub id: u64,
    pub op: u32,
    pub pid: u32,
    pub comm: [u8; 16],
    pub access_path_len: u16,
    pub rule_path_len: u16,
    pub rule_perm: u8,
    pub _pad: [u8; 3],
    pub access_path: [u8; YOLO_PATH_MAX],
    pub rule_path: [u8; YOLO_PATH_MAX],
}

/// Matches `struct yolo_ioc_decision` in the kernel (userspace → kernel).
#[repr(C)]
pub struct YoloIocDecision {
    pub id: u64,
    pub decision: u8,
    pub _pad: [u8; 7],
}

/// Snapshot ioctl flag: skip if no data records since last snapshot.
pub const YOLO_SNAPSHOT_IF_CHANGED: u8 = 1;

/// Matches `struct yolo_ioc_snapshot` in the kernel.
#[repr(C)]
pub struct YoloIocSnapshot {
    pub gen_id: u64,
    pub name_ptr: u64,
    pub name_len: u16,
    pub flags: u8,
    pub _pad: [u8; 5],
}

/// Matches `struct yolo_ioc_travel` in the kernel.
#[repr(C)]
pub struct YoloIocTravel {
    pub target_gen: u64,
    pub new_gen: u64,
    pub tree_len: u64,
    pub tree_ptr: u64,
}

/// Matches `struct yolo_ioc_restore` in the kernel.
#[repr(C)]
pub struct YoloIocRestore {
    pub gen_id: u64,
    pub tree_len: u64,
    pub tree_ptr: u64,
    pub alloc_ino_floor: u32,
    pub cow_ino_floor: u32,
    pub dirty: u8,
    pub _pad: [u8; 7],
}

/// A dequeued ask with owned path data.
#[derive(Debug)]
pub struct Ask {
    pub id: u64,
    pub op: u32,
    pub pid: u32,
    pub comm: [u8; 16],
    pub access_path: String,
    pub rule_path: Option<String>,
    pub rule_perm: Perm,
}

impl Ask {
    pub fn access_path_str(&self) -> &str {
        &self.access_path
    }

    pub fn comm_str(&self) -> &str {
        let end = self.comm.iter().position(|&b| b == 0).unwrap_or(16);
        std::str::from_utf8(&self.comm[..end]).unwrap_or("<invalid>")
    }

    pub fn op_str(&self) -> &'static str {
        match self.op {
            YOLO_OP_READ => "read",
            YOLO_OP_WRITE => "write",
            _ => "unknown",
        }
    }
}

/// Read the head ask via ioctl, without removing it from the queue. Returns an
/// `Ask` with owned path data; `ask_decide` is what resolves and removes it.
pub fn ask_peek(fd: &File) -> std::result::Result<Ask, nix::errno::Errno> {
    let mut req = YoloIocAsk {
        id: 0,
        op: 0,
        pid: 0,
        comm: [0u8; 16],
        access_path_len: 0,
        rule_path_len: 0,
        rule_perm: 0,
        _pad: [0u8; 3],
        access_path: [0u8; YOLO_PATH_MAX],
        rule_path: [0u8; YOLO_PATH_MAX],
    };
    unsafe { ioctl_ask_peek(fd.as_raw_fd(), &mut req) }?;
    let access_path = std::str::from_utf8(&req.access_path[..req.access_path_len as usize])
        .unwrap_or("<invalid>")
        .to_string();
    let rule_path = (req.rule_path_len > 0).then(|| {
        std::str::from_utf8(&req.rule_path[..req.rule_path_len as usize])
            .unwrap_or("<invalid>")
            .to_string()
    });
    let rule_perm = Perm::from_ioctl(req.rule_perm).ok_or(nix::errno::Errno::EINVAL)?;
    Ok(Ask {
        id: req.id,
        op: req.op,
        pid: req.pid,
        comm: req.comm,
        access_path,
        rule_path,
        rule_perm,
    })
}

/// Answer an ask by id via ioctl on a directory fd, which resolves and removes
/// it. Returns `ENOENT` (wrapped) if the ask is already gone — e.g. it timed
/// out before the daemon answered.
pub fn ask_decide_raw(fd: &File, id: u64, decision: u8) -> Result<()> {
    let resp = YoloIocDecision {
        id,
        decision,
        _pad: [0u8; 7],
    };
    unsafe { ioctl_ask_decide(fd.as_raw_fd(), &resp) }.context("ioctl ASK_DECIDE")?;
    Ok(())
}

pub fn ask_decide(fd: &File, id: u64, decision: Decision) -> Result<()> {
    ask_decide_raw(fd, id, decision.to_ioctl())
}

/// Claim the daemon slot before `yolo watch` announces readiness.
///
/// The kernel only recognises a daemon once it has claimed the slot (on its
/// first ASK_PEEK); until then it fast-denies asks as "no daemon". If we
/// printed "watching" before claiming, an operation racing startup would be
/// wrongly denied. A non-blocking ASK_PEEK claims the slot and returns EAGAIN
/// when nothing is queued — no extra ioctl.
///
/// ASK_PEEK does not consume, so an op that raced in the instant after we
/// claimed simply stays queued for the main loop's blocking `ask_peek` — we
/// discard whatever this peek returns and let the loop handle it uniformly.
pub fn claim_daemon(fd: &File) -> Result<()> {
    let raw = fd.as_raw_fd();
    let flags = unsafe { libc::fcntl(raw, libc::F_GETFL) };
    if flags < 0 {
        return Err(std::io::Error::last_os_error()).context("F_GETFL on ctl fd");
    }
    // Toggle O_NONBLOCK around one ASK_PEEK, then restore blocking mode for the loop.
    unsafe { libc::fcntl(raw, libc::F_SETFL, flags | libc::O_NONBLOCK) };
    let res = ask_peek(fd);
    unsafe { libc::fcntl(raw, libc::F_SETFL, flags) };

    match res {
        // Claimed. Ok(ask) means one raced in; it stays queued for the loop.
        Ok(_) | Err(nix::errno::Errno::EAGAIN) => Ok(()),
        Err(nix::errno::Errno::EBUSY) => anyhow::bail!("another yolo watch is already running"),
        Err(e) => Err(anyhow::Error::from(e)).context("claiming daemon slot"),
    }
}

/// Open a directory fd in the mount (the mount root) for control ioctls.
pub fn open(yolo_dir: &Path) -> Result<File> {
    // Control ioctls go to a directory fd in the mount. From outside that's the
    // mount root (`<session>/mnt`); inside the mount that path is hidden, so
    // fall back to "/" (the mount root as seen from within the chroot).
    let mnt = crate::utils::mnt_dir(yolo_dir);
    match OpenOptions::new().read(true).open(&mnt) {
        Ok(f) => Ok(f),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => OpenOptions::new()
            .read(true)
            .open("/")
            .context("opening / for ioctl (inside the mount)"),
        Err(e) => Err(e).context("opening the mount root for ioctl"),
    }
}

/// Open a rule target for [`set_rule`] / [`resolve_rule`]: an O_PATH fd of
/// `path` as seen through the mount. O_PATH skips the filesystem's open hook
/// and read-permission checks, so even `deny`-ruled targets can have their
/// rules managed; symlinks are followed, matching the old kern_path contract.
/// Returns `io::Result` so callers can match on `ErrorKind::NotFound`.
pub fn open_rule_target(path: impl AsRef<Path>) -> std::io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt;
    // The access mode is ignored under O_PATH; read(true) only satisfies
    // OpenOptions' requirement that one is set.
    OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_PATH | libc::O_CLOEXEC)
        .open(path)
}

/// Issue RULE_SET with a raw target fd, returning the bare errno. White-box
/// tests use this to probe the kernel's fd validation with fds no
/// [`open_rule_target`] would produce (closed, outside the mount, unlinked).
pub fn set_rule_raw(
    fd: &File,
    target_fd: i32,
    perm: u8,
) -> std::result::Result<(), nix::errno::Errno> {
    let rule = YoloIocRule {
        fd: target_fd,
        perm,
        _pad: [0u8; 3],
    };
    unsafe { ioctl_rule_set(fd.as_raw_fd(), &rule) }.map(drop)
}

/// Send YOLO_IOC_RULE_SET ioctl. `perm == YOLO_PERM_UNSET` clears the rule.
pub fn set_rule(fd: &File, target: &File, perm: u8) -> Result<()> {
    set_rule_raw(fd, target.as_raw_fd(), perm).context("ioctl RULE_SET")?;
    Ok(())
}

/// Send YOLO_IOC_RULE_RESOLVE ioctl. Returns the effective perm the kernel
/// would enforce for `target` (resolved by walking up to the nearest rule).
pub fn resolve_rule(fd: &File, target: &File) -> Result<u8> {
    let mut rule = YoloIocRule {
        fd: target.as_raw_fd(),
        perm: YOLO_PERM_UNSET,
        _pad: [0u8; 3],
    };
    unsafe { ioctl_rule_resolve(fd.as_raw_fd(), &mut rule) }.context("ioctl RULE_RESOLVE")?;
    Ok(rule.perm)
}

/// Send YOLO_IOC_TRAVEL ioctl: travel to a generation (0 = base, >=1 =
/// snapshot/travel) by injecting that generation's serialized DirTree. Returns
/// the new generation assigned. Commit/abort cleanup uses [`restore`], not travel.
pub fn travel(fd: &File, target_gen: u64, tree_buf: &[u8]) -> Result<u64> {
    // An empty DirTree still serializes to 2 bytes (the root child count),
    // so tree_buf is never empty here.
    let mut hdr = YoloIocTravel {
        target_gen,
        new_gen: 0,
        tree_len: tree_buf.len() as u64,
        tree_ptr: tree_buf.as_ptr() as u64,
    };
    unsafe { ioctl_travel(fd.as_raw_fd(), &mut hdr) }.context("ioctl TRAVEL")?;
    Ok(hdr.new_gen)
}

/// Replace the kernel's in-memory staged view without writing a journal record.
/// Both floors are "max S-record ino" thresholds: `alloc_ino_floor` over the
/// full journal (the allocator resumes above it — dead/deleted inos still
/// occupy the store), `cow_ino_floor` at the latest marker (injected inos
/// above it resume write-in-place; at or below, they are snapshot-retained
/// and re-COW on first write).
pub fn restore(
    fd: &File,
    gen_id: u64,
    dirty: bool,
    alloc_ino_floor: u32,
    cow_ino_floor: u32,
    tree_buf: &[u8],
) -> Result<()> {
    let hdr = YoloIocRestore {
        gen_id,
        tree_len: tree_buf.len() as u64,
        tree_ptr: tree_buf.as_ptr() as u64,
        alloc_ino_floor,
        cow_ino_floor,
        dirty: dirty.into(),
        _pad: [0; 7],
    };
    unsafe { ioctl_restore(fd.as_raw_fd(), &hdr) }.context("ioctl RESTORE")?;
    Ok(())
}

/// Send YOLO_IOC_SNAPSHOT ioctl. Returns the assigned gen, or 0 if
/// skipped due to `YOLO_SNAPSHOT_IF_CHANGED` with no pending changes.
pub fn snapshot(fd: &File, name: &str, flags: u8) -> Result<u64> {
    let name_bytes = name.as_bytes();
    let name_len: u16 = name_bytes
        .len()
        .try_into()
        .context("snapshot name too long")?;
    let mut mrk = YoloIocSnapshot {
        gen_id: 0,
        name_ptr: name_bytes.as_ptr() as u64,
        name_len,
        flags,
        _pad: [0u8; 5],
    };
    unsafe { ioctl_snapshot(fd.as_raw_fd(), &mut mrk) }.context("ioctl SNAPSHOT")?;
    Ok(mrk.gen_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn struct_sizes() {
        // Must match the kernel struct sizes for binary protocol compat
        assert_eq!(size_of::<YoloIocAsk>(), 552);
        assert_eq!(size_of::<YoloIocDecision>(), 16);
        assert_eq!(size_of::<YoloIocRule>(), 8);
        assert_eq!(size_of::<YoloIocSnapshot>(), 24);
        assert_eq!(size_of::<YoloIocTravel>(), 32);
        assert_eq!(size_of::<YoloIocRestore>(), 40);
    }

    #[test]
    fn ask_helpers() {
        let req = Ask {
            id: 1,
            op: YOLO_OP_READ,
            pid: 42,
            comm: {
                let mut c = [0u8; 16];
                c[..4].copy_from_slice(b"bash");
                c
            },
            access_path: "hello".into(),
            rule_path: Some("/tmp".into()),
            rule_perm: Perm::Ask,
        };
        assert_eq!(req.access_path_str(), "hello");
        assert_eq!(req.comm_str(), "bash");
        assert_eq!(req.op_str(), "read");
    }

    #[test]
    fn op_str_all_variants() {
        let mk = |op| Ask {
            id: 0,
            op,
            pid: 0,
            comm: [0u8; 16],
            access_path: String::new(),
            rule_path: None,
            rule_perm: Perm::Ask,
        };
        assert_eq!(mk(YOLO_OP_READ).op_str(), "read");
        assert_eq!(mk(YOLO_OP_WRITE).op_str(), "write");
        assert_eq!(mk(99).op_str(), "unknown");
    }

    #[test]
    fn open_rule_target_missing_path_is_not_found() {
        let err = open_rule_target("/nonexistent/rule/target").unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::NotFound);
    }

    #[test]
    fn open_rule_target_opens_unreadable_file() {
        // O_PATH must succeed even where a normal read open would fail.
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("yolo-opath-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("secret");
        std::fs::write(&path, b"x").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).unwrap();
        let opened = open_rule_target(path.to_str().unwrap());
        std::fs::remove_dir_all(&dir).unwrap();
        let f = opened.unwrap();
        assert!(f.as_raw_fd() >= 0);
    }
}

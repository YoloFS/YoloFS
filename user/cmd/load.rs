// yolo CLI — load.rs
//
// `yolo load`   — load the kernel module.
// `yolo unload` — unmount all sessions and unload the kernel module.
// `yolo reload` — unload then reload the kernel module.

use crate::report;
use anyhow::{Context, Result};
use std::fs::File;
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// How long `unload` waits for the module's refcount to reach zero, and how
/// long it sleeps between `delete_module(2)` attempts.
const UNLOAD_DEADLINE: Duration = Duration::from_secs(2);
const UNLOAD_RETRY_STEP: Duration = Duration::from_millis(50);

/// Check if the YoloFS kernel module is loaded.
pub fn is_loaded() -> bool {
    Path::new("/sys/module/yolofs").exists()
}

/// Load the kernel module if not already loaded. Returns `true` if freshly loaded.
pub fn load() -> Result<bool> {
    if is_loaded() {
        return Ok(false);
    }

    let ko_path = find_ko().context("cannot find yolofs.ko — build it with `make kmod`")?;

    report::info(format!("loading kernel module {}", ko_path.display()));

    // Load via finit_module(2) using CAP_SYS_MODULE (a file capability) rather
    // than shelling out to `sudo insmod` — keeps every privileged op on the
    // capability model and drops the runtime sudo dependency.
    let file = File::open(&ko_path).with_context(|| format!("opening {}", ko_path.display()))?;
    let ret = unsafe { libc::syscall(libc::SYS_finit_module, file.as_raw_fd(), c"".as_ptr(), 0) };
    if ret != 0 {
        return Err(std::io::Error::last_os_error()).context("finit_module (loading yolofs.ko)");
    }

    Ok(true)
}

/// Unmount all YoloFS sessions and unload the kernel module.
pub fn unload() -> Result<()> {
    unmount_all()?;

    if !is_loaded() {
        report::hint("kernel module not loaded");
        return Ok(());
    }

    report::info("unloading kernel module");

    // Unload via delete_module(2) using CAP_SYS_MODULE. O_NONBLOCK matches
    // rmmod's default: fail rather than block if the module is still in use
    // (a blocking call would hang forever on a genuinely held reference).
    //
    // unmount_all above drops all references, but umount(2) only detaches the
    // mount from the namespace — the superblock teardown that releases the
    // module refcount can run asynchronously (e.g. deferred fput on a kernel
    // workqueue), returning EAGAIN/EBUSY for a moment after umount returns.
    // Retry briefly to cover that window.
    retry_busy(UNLOAD_DEADLINE, UNLOAD_RETRY_STEP, || {
        let ret = unsafe {
            libc::syscall(
                libc::SYS_delete_module,
                c"yolofs".as_ptr(),
                libc::O_NONBLOCK,
            )
        };
        if ret == 0 {
            return Attempt::Done(());
        }
        let err = std::io::Error::last_os_error();
        match err.raw_os_error() {
            Some(libc::EAGAIN) | Some(libc::EBUSY) => Attempt::Busy(err),
            _ => Attempt::Fail(err),
        }
    })
    .map_err(|err| match err.raw_os_error() {
        // Deadline expired while busy: report the live refcount instead of a
        // raw errno.
        Some(libc::EAGAIN) | Some(libc::EBUSY) => anyhow::anyhow!(
            "module still has {} reference(s) after {}s",
            module_refcnt().unwrap_or_else(|| "?".into()),
            UNLOAD_DEADLINE.as_secs(),
        ),
        _ => anyhow::Error::new(err),
    })
    .context("delete_module (unloading yolofs)")
}

/// One attempt of an operation retried by [`retry_busy`].
enum Attempt<T, E> {
    /// Succeeded; stop retrying.
    Done(T),
    /// Failed but may succeed shortly; retry until the deadline.
    Busy(E),
    /// Failed permanently; stop retrying.
    Fail(E),
}

/// Run `f` until it returns [`Attempt::Done`] or [`Attempt::Fail`], sleeping
/// `step` between [`Attempt::Busy`] attempts. Returns the last error once
/// `deadline` has elapsed.
fn retry_busy<T, E>(
    deadline: Duration,
    step: Duration,
    mut f: impl FnMut() -> Attempt<T, E>,
) -> Result<T, E> {
    let start = Instant::now();
    loop {
        match f() {
            Attempt::Done(v) => return Ok(v),
            Attempt::Fail(e) => return Err(e),
            Attempt::Busy(e) => {
                if start.elapsed() >= deadline {
                    return Err(e);
                }
                std::thread::sleep(step);
            }
        }
    }
}

/// Read the module's live reference count from sysfs.
fn module_refcnt() -> Option<String> {
    std::fs::read_to_string("/sys/module/yolofs/refcnt")
        .ok()
        .map(|s| s.trim().to_string())
}

/// Unload then reload the kernel module.
pub fn reload() -> Result<()> {
    if is_loaded() {
        unload()?;
    }
    load()?;
    Ok(())
}

/// Find the .ko file: dev build directory, then system install path.
fn find_ko() -> Option<PathBuf> {
    let build_path = dev_ko_path()?;
    if build_path.exists() {
        return Some(build_path);
    }

    let mut uts = unsafe { std::mem::zeroed::<libc::utsname>() };
    if unsafe { libc::uname(&mut uts) } != 0 {
        return None;
    }
    let release = unsafe { std::ffi::CStr::from_ptr(uts.release.as_ptr()) }
        .to_str()
        .ok()?;
    let system_path = PathBuf::from(format!("/lib/modules/{release}/extra/yolofs.ko"));
    if system_path.exists() {
        return Some(system_path);
    }

    None
}

fn dev_ko_path() -> Option<PathBuf> {
    let cwd = std::env::current_dir().ok()?;
    Some(cwd.join("build").join("yolofs.ko"))
}

/// Find all active YoloFS sessions by reading /proc/mounts, as `(source,
/// mountpoint)` pairs.
fn find_yolo_mounts() -> Vec<(String, String)> {
    let Ok(content) = std::fs::read_to_string("/proc/mounts") else {
        return Vec::new();
    };
    parse_mounts(&content)
}

/// Parse /proc/mounts content into `(source, mountpoint)` pairs for YoloFS
/// entries. Both columns matter: the mountpoint is what we actually unmount
/// (the kernel's authoritative location, valid even if `.yolofs/` was deleted),
/// and the source locates the `.yolofs/` dir for best-effort cleanup.
fn parse_mounts(content: &str) -> Vec<(String, String)> {
    content
        .lines()
        .filter_map(|line| {
            let mut cols = line.split_whitespace();
            let source = cols.next()?;
            let mountpoint = cols.next()?;
            let fstype = cols.next()?;
            (fstype == "yolofs").then(|| (source.to_string(), mountpoint.to_string()))
        })
        .collect()
}

/// Unmount all active YoloFS sessions, drop their `cwd` symlinks, then sweep
/// stale runtime mountpoint dirs.
///
/// Unmounts each session by the mountpoint the *kernel* reports in
/// /proc/mounts, not the one recorded in `.yolofs/mnt`. If the user deleted the
/// project dir before unmounting, that symlink is gone but the kernel mount —
/// and the module reference it holds — lives on; going straight to the
/// kernel-reported mountpoint tears it down regardless, so `delete_module`
/// isn't blocked by an orphaned mount.
fn unmount_all() -> Result<()> {
    for (source, mountpoint) in find_yolo_mounts() {
        report::info(format!("unmounting {mountpoint}"));
        crate::cmd::mount::umount_or_prompt(Path::new(&mountpoint))
            .with_context(|| format!("unmounting {mountpoint}"))?;
        // The cwd symlink points into the (now gone) mount. Drop it if the
        // project dir still exists; harmless if `.yolofs/` was already deleted.
        let _ = std::fs::remove_file(Path::new(&source).join("cwd"));
    }
    sweep_runtime_mountpoints();
    Ok(())
}

/// Remove empty, now-unmounted leftover mountpoint dirs under the per-user
/// runtime base. A session whose project dir was deleted before unmount leaves
/// its mountpoint dir behind (the `.yolofs/mnt` symlink that pointed at it is
/// gone), so these accumulate. `rmdir` fails on a non-empty dir — an active
/// mountpoint shows the `/` view and so is never empty — which is exactly the
/// safety we want: only truly stale empties are removed.
///
/// This runs only as the tail of `unload`, a deliberate global teardown that
/// unmounts every session anyway. It therefore assumes no `yolo mount` is
/// racing it — such a mount's freshly `mkdtemp`'d (still-empty) mountpoint could
/// be swept, but racing a new mount against a global unload is unsound
/// regardless of the sweep.
fn sweep_runtime_mountpoints() {
    let Ok(entries) = std::fs::read_dir(crate::utils::runtime_base()) else {
        return;
    };
    for entry in entries.flatten() {
        let _ = std::fs::remove_dir(entry.path());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn find_ko_returns_existing_path() {
        if let Some(path) = find_ko() {
            assert!(
                path.exists(),
                "find_ko returned non-existent path: {}",
                path.display()
            );
            assert!(
                path.to_string_lossy().ends_with("yolofs.ko"),
                "find_ko returned unexpected file: {}",
                path.display()
            );
        }
    }

    #[test]
    fn find_ko_prefers_build_dir() {
        let build_path = dev_ko_path().expect("dev ko path should resolve");
        if build_path.exists() {
            let found = find_ko().expect("find_ko should succeed when build dir exists");
            assert_eq!(
                found, build_path,
                "should prefer build/yolofs.ko over system path"
            );
        }
    }

    #[test]
    fn parse_mounts_extracts_source_and_mountpoint() {
        let content = "\
/dev/sda1 / ext4 rw,relatime 0 0
/home/user/.yolofs/abc /run/user/1000/yolofs/abc yolofs rw 0 0
proc /proc proc rw,nosuid 0 0
/tmp/project/.yolofs /run/user/1000/yolofs/xyz yolofs rw 0 0
";
        let mounts = parse_mounts(content);
        assert_eq!(
            mounts,
            vec![
                (
                    "/home/user/.yolofs/abc".to_string(),
                    "/run/user/1000/yolofs/abc".to_string()
                ),
                (
                    "/tmp/project/.yolofs".to_string(),
                    "/run/user/1000/yolofs/xyz".to_string()
                ),
            ]
        );
    }

    #[test]
    fn parse_mounts_empty_input() {
        assert!(parse_mounts("").is_empty());
    }

    #[test]
    fn parse_mounts_no_yolo_entries() {
        let content = "\
/dev/sda1 / ext4 rw,relatime 0 0
proc /proc proc rw,nosuid 0 0
";
        assert!(parse_mounts(content).is_empty());
    }

    #[test]
    fn parse_mounts_ignores_substring_matches() {
        // "myolofs" contains "yolofs" but is a different fstype, so it must not match.
        let content = "src /mnt myolofs rw 0 0\n";
        assert!(parse_mounts(content).is_empty());
    }

    #[test]
    fn parse_mounts_skips_truncated_lines() {
        // Lines without all three of source/mountpoint/fstype are skipped rather
        // than panicking (the `?` chain bails on the missing column). A complete
        // yolofs line on either side is still parsed.
        let content = "\
/tmp/a/.yolofs /run/user/1000/yolofs/a yolofs rw 0 0
src /mnt
onlyone

/tmp/b/.yolofs /run/user/1000/yolofs/b yolofs rw 0 0
";
        let mounts = parse_mounts(content);
        assert_eq!(
            mounts,
            vec![
                (
                    "/tmp/a/.yolofs".to_string(),
                    "/run/user/1000/yolofs/a".to_string()
                ),
                (
                    "/tmp/b/.yolofs".to_string(),
                    "/run/user/1000/yolofs/b".to_string()
                ),
            ]
        );
    }

    #[test]
    fn retry_busy_stops_on_success() {
        let mut calls = 0;
        let result: Result<u32, &str> = retry_busy(Duration::from_secs(1), Duration::ZERO, || {
            calls += 1;
            Attempt::Done(42)
        });
        assert_eq!(result, Ok(42));
        assert_eq!(calls, 1, "should not retry after success");
    }

    #[test]
    fn retry_busy_retries_until_success() {
        let mut calls = 0;
        let result: Result<u32, &str> = retry_busy(Duration::from_secs(1), Duration::ZERO, || {
            calls += 1;
            if calls < 3 {
                Attempt::Busy("busy")
            } else {
                Attempt::Done(42)
            }
        });
        assert_eq!(result, Ok(42));
        assert_eq!(calls, 3, "should retry while busy");
    }

    #[test]
    fn retry_busy_gives_up_at_deadline() {
        let deadline = Duration::from_millis(20);
        let start = Instant::now();
        let mut calls = 0;
        let result: Result<(), &str> = retry_busy(deadline, Duration::from_millis(1), || {
            calls += 1;
            Attempt::Busy("busy")
        });
        assert_eq!(result, Err("busy"));
        assert!(calls > 1, "should attempt more than once before giving up");
        assert!(
            start.elapsed() >= deadline,
            "should keep retrying until the deadline"
        );
    }

    #[test]
    fn retry_busy_fails_fast_on_hard_error() {
        let mut calls = 0;
        let result: Result<(), &str> = retry_busy(Duration::from_secs(1), Duration::ZERO, || {
            calls += 1;
            Attempt::Fail("hard error")
        });
        assert_eq!(result, Err("hard error"));
        assert_eq!(calls, 1, "should not retry a hard error");
    }

    #[test]
    fn find_yolo_mounts_matches_proc_mounts() {
        let mounts = find_yolo_mounts();
        let content = std::fs::read_to_string("/proc/mounts").unwrap_or_default();
        let expected = parse_mounts(&content);
        assert_eq!(mounts, expected);
    }
}

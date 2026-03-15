// agfs CLI — kmod.rs
//
// `agfs load`   — load the kernel module.
// `agfs unload` — unmount all sessions and unload the kernel module.
// `agfs reload` — unload then reload the kernel module.

use anyhow::{Context, Result};
use colored::Colorize;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Check if the agfs kernel module is loaded.
pub fn is_loaded() -> bool {
    Path::new("/sys/module/agfs").exists()
}

/// Load the kernel module if not already loaded.
pub fn load() -> Result<()> {
    if is_loaded() {
        eprintln!("{} kernel module already loaded", "agfs:".green());
        return Ok(());
    }

    let ko_path = find_ko().context("cannot find agfs.ko — build it with `make kmod`")?;

    eprintln!(
        "{} {}",
        "agfs: loading kernel module".green(),
        ko_path.display()
    );

    let output = Command::new("sudo")
        .args(["insmod", &ko_path.to_string_lossy()])
        .output()
        .context("running sudo insmod")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("insmod failed: {stderr}");
    }

    Ok(())
}

/// Unmount all agfs sessions and unload the kernel module.
pub fn unload() -> Result<()> {
    unmount_all()?;

    if !is_loaded() {
        eprintln!("{} kernel module not loaded", "agfs:".green());
        return Ok(());
    }

    eprintln!("{}", "agfs: unloading kernel module".green());

    let output = Command::new("sudo")
        .args(["rmmod", "agfs"])
        .output()
        .context("running sudo rmmod")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("rmmod failed: {stderr}");
    }

    Ok(())
}

/// Unload then reload the kernel module.
pub fn reload() -> Result<()> {
    unload()?;
    load()
}

/// Find the .ko file: dev build directory, then system install path.
fn find_ko() -> Option<PathBuf> {
    let cwd_path = Path::new("kmod/build/agfs.ko");
    if cwd_path.exists() {
        return Some(cwd_path.to_path_buf());
    }

    let mut uts = unsafe { std::mem::zeroed::<libc::utsname>() };
    if unsafe { libc::uname(&mut uts) } != 0 {
        return None;
    }
    let release = unsafe { std::ffi::CStr::from_ptr(uts.release.as_ptr()) }
        .to_str()
        .ok()?;
    let system_path = PathBuf::from(format!("/lib/modules/{release}/extra/agfs.ko"));
    if system_path.exists() {
        return Some(system_path);
    }

    None
}

/// Find all active agfs session directories by reading /proc/mounts.
fn find_agfs_dirs() -> Vec<String> {
    let Ok(content) = std::fs::read_to_string("/proc/mounts") else {
        return Vec::new();
    };
    content
        .lines()
        .filter(|line| line.contains(" agfs "))
        .filter_map(|line| line.split_whitespace().next())
        .map(String::from)
        .collect()
}

/// Unmount all active agfs sessions.
fn unmount_all() -> Result<()> {
    for agfs_dir in find_agfs_dirs() {
        eprintln!("{} {}", "agfs: unmounting".green(), agfs_dir);
        crate::mount::unmount_at(Path::new(&agfs_dir))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn is_loaded_checks_sysfs() {
        // /sys/module/agfs exists iff the module is loaded.
        // We can't control this in a unit test, but verify it returns
        // a consistent result matching the sysfs entry.
        let expected = Path::new("/sys/module/agfs").exists();
        assert_eq!(is_loaded(), expected);
    }

    #[test]
    fn find_ko_returns_existing_path() {
        // If find_ko returns Some, the path must actually exist.
        if let Some(path) = find_ko() {
            assert!(
                path.exists(),
                "find_ko returned non-existent path: {}",
                path.display()
            );
            assert!(
                path.to_string_lossy().ends_with("agfs.ko"),
                "find_ko returned unexpected file: {}",
                path.display()
            );
        }
    }

    #[test]
    fn find_ko_prefers_build_dir() {
        // If kmod/build/agfs.ko exists (dev environment), it should be preferred.
        let cwd_path = Path::new("kmod/build/agfs.ko");
        if cwd_path.exists() {
            let found = find_ko().expect("find_ko should succeed when build dir exists");
            assert_eq!(
                found,
                cwd_path.to_path_buf(),
                "should prefer kmod/build/ over system path"
            );
        }
    }

    #[test]
    fn find_agfs_dirs_returns_valid_paths() {
        // Each returned path should be a directory (or at least parseable).
        let dirs = find_agfs_dirs();
        for dir in &dirs {
            assert!(!dir.is_empty(), "find_agfs_dirs returned an empty string");
            // The source column in /proc/mounts for agfs is the .agfs/ dir
            assert!(
                Path::new(dir).is_absolute() || dir.contains(".agfs"),
                "unexpected agfs mount source: {dir}"
            );
        }
    }

    #[test]
    fn find_agfs_dirs_parses_proc_mounts() {
        // Verify we can read /proc/mounts without panicking, and the
        // result count matches a manual grep.
        let dirs = find_agfs_dirs();
        let content = fs::read_to_string("/proc/mounts").unwrap_or_default();
        let expected_count = content.lines().filter(|l| l.contains(" agfs ")).count();
        assert_eq!(
            dirs.len(),
            expected_count,
            "find_agfs_dirs count should match /proc/mounts grep"
        );
    }

    #[test]
    fn uname_returns_valid_release() {
        // Verify our libc::uname usage produces a valid kernel release string.
        let mut uts = unsafe { std::mem::zeroed::<libc::utsname>() };
        assert_eq!(unsafe { libc::uname(&mut uts) }, 0);
        let release = unsafe { std::ffi::CStr::from_ptr(uts.release.as_ptr()) }
            .to_str()
            .expect("release should be valid UTF-8");
        assert!(!release.is_empty(), "kernel release should not be empty");
        // Kernel release looks like "6.8.0-101-generic"
        assert!(
            release.contains('.'),
            "kernel release should contain dots: {release}"
        );
    }
}

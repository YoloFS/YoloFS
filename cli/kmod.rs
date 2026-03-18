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

/// Load the kernel module if not already loaded. Returns `true` if freshly loaded.
pub fn load() -> Result<bool> {
    if is_loaded() {
        return Ok(false);
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

    Ok(true)
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
    load()?;
    Ok(())
}

/// Find the .ko file: dev build directory, then system install path.
fn find_ko() -> Option<PathBuf> {
    let cwd_path = Path::new("target/kmod/agfs.ko");
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
    parse_mounts(&content)
}

/// Parse /proc/mounts content and return the source column for agfs entries.
fn parse_mounts(content: &str) -> Vec<String> {
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

    #[test]
    fn find_ko_returns_existing_path() {
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
        let cwd_path = Path::new("target/kmod/agfs.ko");
        if cwd_path.exists() {
            let found = find_ko().expect("find_ko should succeed when build dir exists");
            assert_eq!(
                found,
                cwd_path.to_path_buf(),
                "should prefer target/kmod/ over system path"
            );
        }
    }

    #[test]
    fn parse_mounts_extracts_agfs_sources() {
        let content = "\
/dev/sda1 / ext4 rw,relatime 0 0
/home/user/.agfs/abc /.agfs/abc/mnt agfs rw 0 0
proc /proc proc rw,nosuid 0 0
/tmp/project/.agfs /.agfs/mnt agfs rw 0 0
";
        let dirs = parse_mounts(content);
        assert_eq!(dirs, vec!["/home/user/.agfs/abc", "/tmp/project/.agfs",]);
    }

    #[test]
    fn parse_mounts_empty_input() {
        assert!(parse_mounts("").is_empty());
    }

    #[test]
    fn parse_mounts_no_agfs_entries() {
        let content = "\
/dev/sda1 / ext4 rw,relatime 0 0
proc /proc proc rw,nosuid 0 0
";
        assert!(parse_mounts(content).is_empty());
    }

    #[test]
    fn parse_mounts_ignores_substring_matches() {
        // "magfs" contains "agfs" but should not match " agfs "
        let content = "src /mnt magfs rw 0 0\n";
        assert!(parse_mounts(content).is_empty());
    }

    #[test]
    fn find_agfs_dirs_matches_proc_mounts() {
        let dirs = find_agfs_dirs();
        let content = std::fs::read_to_string("/proc/mounts").unwrap_or_default();
        let expected = parse_mounts(&content);
        assert_eq!(dirs, expected);
    }
}

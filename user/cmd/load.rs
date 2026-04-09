// yolo CLI — load.rs
//
// `yolo load`   — load the kernel module.
// `yolo unload` — unmount all sessions and unload the kernel module.
// `yolo reload` — unload then reload the kernel module.

use anyhow::{Context, Result};
use colored::Colorize;
use std::path::{Path, PathBuf};
use std::process::Command;

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

    eprintln!(
        "{} {}",
        "yolo: loading kernel module".green(),
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

/// Unmount all YoloFS sessions and unload the kernel module.
pub fn unload() -> Result<()> {
    unmount_all()?;

    if !is_loaded() {
        eprintln!("{} kernel module not loaded", "yolo:".green());
        return Ok(());
    }

    eprintln!("{}", "yolo: unloading kernel module".green());

    let output = Command::new("sudo")
        .args(["rmmod", "yolofs"])
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

/// Find all active YoloFS session directories by reading /proc/mounts.
fn find_yolo_dirs() -> Vec<String> {
    let Ok(content) = std::fs::read_to_string("/proc/mounts") else {
        return Vec::new();
    };
    parse_mounts(&content)
}

/// Parse /proc/mounts content and return the source column for YoloFS entries.
fn parse_mounts(content: &str) -> Vec<String> {
    content
        .lines()
        .filter(|line| line.contains(" yolofs "))
        .filter_map(|line| line.split_whitespace().next())
        .map(String::from)
        .collect()
}

/// Unmount all active YoloFS sessions.
fn unmount_all() -> Result<()> {
    for yolo_dir in find_yolo_dirs() {
        eprintln!("{} {}", "yolo: unmounting".green(), yolo_dir);
        crate::cmd::mount::unmount_at(Path::new(&yolo_dir))?;
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
    fn parse_mounts_extracts_yolo_sources() {
        let content = "\
/dev/sda1 / ext4 rw,relatime 0 0
/home/user/.yolofs/abc /.yolofs/abc/mnt yolofs rw 0 0
proc /proc proc rw,nosuid 0 0
/tmp/project/.yolofs /.yolofs/mnt yolofs rw 0 0
";
        let dirs = parse_mounts(content);
        assert_eq!(
            dirs,
            vec!["/home/user/.yolofs/abc", "/tmp/project/.yolofs",]
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
        // "myolofs" contains "yolofs" but should not match " yolofs "
        let content = "src /mnt myolofs rw 0 0\n";
        assert!(parse_mounts(content).is_empty());
    }

    #[test]
    fn find_yolo_dirs_matches_proc_mounts() {
        let dirs = find_yolo_dirs();
        let content = std::fs::read_to_string("/proc/mounts").unwrap_or_default();
        let expected = parse_mounts(&content);
        assert_eq!(dirs, expected);
    }
}

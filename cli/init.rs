// agfs CLI — init.rs
//
// `agfs init`   — create agfs.toml and load the kernel module.
// `agfs deinit` — unload the kernel module.

use anyhow::{Context, Result};
use colored::Colorize;
use std::env;
use std::path::Path;
use std::process::Command;

use crate::config::Config;

/// Check if the agfs kernel module is already loaded.
fn is_kmod_loaded() -> bool {
    Path::new("/sys/module/agfs").exists()
}

/// Find the .ko file: check kmod/build/ relative to cwd, then next to the binary.
fn find_kmod() -> Option<std::path::PathBuf> {
    let cwd_path = Path::new("kmod/build/agfs.ko");
    if cwd_path.exists() {
        return Some(cwd_path.to_path_buf());
    }

    if let Ok(exe) = env::current_exe() {
        let beside_exe = exe.parent()?.join("agfs.ko");
        if beside_exe.exists() {
            return Some(beside_exe);
        }
    }

    None
}

/// Load the agfs kernel module via insmod.
fn load_kmod() -> Result<()> {
    if is_kmod_loaded() {
        eprintln!("{} {}", "agfs:".green(), "kernel module already loaded");
        return Ok(());
    }

    let ko_path = find_kmod()
        .context("cannot find agfs.ko — build it with `make kmod`")?;

    eprintln!("{} {}", "agfs: loading kernel module".green(), ko_path.display());

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

/// Unload the agfs kernel module via rmmod.
fn unload_kmod() -> Result<()> {
    if !is_kmod_loaded() {
        eprintln!("{} {}", "agfs:".green(), "kernel module not loaded");
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

/// Find all active agfs session directories by reading /proc/mounts.
/// The source (first column) is the .agfs/ directory.
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
fn unmount_all() {
    for agfs_dir in find_agfs_dirs() {
        eprintln!("{} {}", "agfs: unmounting".green(), agfs_dir);
        crate::mount::unmount_at(Path::new(&agfs_dir));
    }
}

/// Create agfs.toml and load the kernel module.
pub fn init() -> Result<()> {
    let cwd = env::current_dir().context("getting cwd")?;
    let config_path = cwd.join("agfs.toml");

    if config_path.exists() {
        eprintln!("{}", "agfs.toml already exists".yellow());
    } else {
        Config::default().save(&config_path)?;
        eprintln!("{} {}", "created".green().bold(), config_path.display());
    }

    load_kmod()?;

    Ok(())
}

/// Unload then reload the kernel module.
pub fn reinit() -> Result<()> {
    deinit()?;
    init()
}

/// Unmount all agfs mounts and unload the kernel module.
pub fn deinit() -> Result<()> {
    unmount_all();
    unload_kmod()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_kmod_loaded_returns_bool() {
        let _ = is_kmod_loaded();
    }

    #[test]
    fn find_kmod_does_not_panic() {
        let _ = find_kmod();
    }
}

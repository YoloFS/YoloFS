// agfs CLI — mount.rs
//
// `agfs mount` — create .agfs/ layout and mount the filesystem.

use anyhow::{Context, Result};
use colored::Colorize;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

/// Create .agfs/ layout and mount the filesystem.
pub fn run() -> Result<()> {
    let agfs_dir = agfs_dir_path()?;
    setup_agfs_dir(&agfs_dir)?;
    do_mount(&agfs_dir)?;
    apply_rules_from_config(&agfs_dir)?;
    Ok(())
}

/// Return the .agfs/ path for the current directory.
pub fn agfs_dir_path() -> Result<PathBuf> {
    let cwd = env::current_dir().context("getting cwd")?;
    Ok(cwd.join(".agfs"))
}

pub fn setup_agfs_dir(agfs_dir: &Path) -> Result<()> {
    fs::create_dir_all(agfs_dir.join("staging"))
        .context("creating .agfs/staging/")?;
    fs::create_dir_all(agfs_dir.join("mnt"))
        .context("creating .agfs/mnt/")?;
    Ok(())
}

pub fn do_mount(agfs_dir: &Path) -> Result<()> {
    let mnt = agfs_dir.join("mnt");
    let storage = agfs_dir.to_string_lossy().to_string();
    let mount_data = format!("storage={}", storage);

    nix::mount::mount(
        Some("none"),
        &mnt,
        Some("agfs"),
        nix::mount::MsFlags::empty(),
        Some(mount_data.as_str()),
    )
    .context("mounting agfs (is the kernel module loaded?)")?;

    // Mount fresh pseudo-filesystems so they bypass agfs
    let pseudos: &[(&str, &str)] = &[
        ("dev", "devtmpfs"),
        ("proc", "proc"),
        ("sys", "sysfs"),
    ];
    for &(dir, fstype) in pseudos {
        let target = mnt.join(dir);
        if target.exists() {
            nix::mount::mount(
                Some(fstype),
                &target,
                Some(fstype),
                nix::mount::MsFlags::empty(),
                None::<&str>,
            )
            .with_context(|| format!("mounting {fstype} at {dir}"))?;
        }
    }

    eprintln!("{} {}", "agfs: mounted at".green(), mnt.display());
    Ok(())
}

/// Read agfs.toml from CWD and apply [rules] via ioctl.
fn apply_rules_from_config(agfs_dir: &Path) -> Result<()> {
    let cwd = agfs_dir.parent().unwrap_or(Path::new("."));
    let config_path = cwd.join("agfs.toml");
    if !config_path.exists() {
        return Ok(());
    }

    let content = fs::read_to_string(&config_path).context("reading agfs.toml")?;
    let doc: toml::Table = content.parse().unwrap_or_default();

    let rules = match doc.get("rules") {
        Some(toml::Value::Table(t)) => t,
        _ => return Ok(()),
    };

    let ctl_file = crate::ctl::open_ctl(agfs_dir)?;
    let mut count = 0;

    for (path, value) in rules {
        let perm_str = match value.as_str() {
            Some(s) => s,
            None => continue,
        };
        let perm = match crate::ctl::perm_from_str(perm_str) {
            Some(p) => p,
            None => {
                eprintln!("agfs: skipping invalid rule: {} = {}", path, perm_str);
                continue;
            }
        };

        // Resolve relative paths against CWD
        let resolved = if path.starts_with('/') {
            path.to_string()
        } else {
            cwd.join(path).to_string_lossy().to_string()
        };

        if let Err(e) = crate::ctl::ioctl_add_rule(&ctl_file, &resolved, perm) {
            eprintln!("agfs: rule {} = {}: {}", path, perm_str, e);
        } else {
            count += 1;
        }
    }

    if count > 0 {
        eprintln!("{}", format!("agfs: applied {count} rule(s) from agfs.toml").cyan());
    }
    Ok(())
}

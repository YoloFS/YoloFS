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

    let config_path = agfs_dir.join("config.toml");
    if !config_path.exists() {
        let default_config = r#"[mount]
ask_timeout = 0
ask_default = "deny"

[rules]
"#;
        fs::write(&config_path, default_config)
            .context("writing default config.toml")?;
    }

    Ok(())
}

pub fn do_mount(agfs_dir: &Path) -> Result<()> {
    let mnt = agfs_dir.join("mnt");
    let storage = agfs_dir.to_string_lossy().to_string();
    let mount_data = format!("storage={},nogating", storage);

    nix::mount::mount(
        Some("none"),
        &mnt,
        Some("agfs"),
        nix::mount::MsFlags::empty(),
        Some(mount_data.as_str()),
    )
    .context("mounting agfs (is the kernel module loaded?)")?;

    eprintln!("{} {}", "agfs: mounted at".green(), mnt.display());
    Ok(())
}

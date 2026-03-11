// agfs CLI — rule.rs
//
// `agfs rule add <path> <perm>` / `agfs rule remove <path>`

use crate::ctl::{self, perm_from_str};
use anyhow::Result;
use colored::Colorize;
use std::path::Path;

/// Resolve a path: relative paths are resolved against the session root
/// (parent of .agfs/), absolute paths are used as-is.
fn resolve_rule_path(path: &str) -> Result<String> {
    if path.starts_with('/') {
        return Ok(path.to_string());
    }
    let agfs = ctl::agfs_dir()?;
    let session_root = agfs.parent().unwrap_or(Path::new("."));
    let full = session_root.join(path);
    Ok(full.to_string_lossy().to_string())
}

pub fn add(path: &str, perm_str: &str) -> Result<()> {
    let perm = perm_from_str(perm_str)
        .ok_or_else(|| anyhow::anyhow!("unknown permission: {perm_str}"))?;

    let resolved = resolve_rule_path(path)?;
    let agfs = ctl::agfs_dir()?;
    let ctl_file = ctl::open_ctl(&agfs)?;

    ctl::ioctl_add_rule(&ctl_file, &resolved, perm)?;
    eprintln!("{} {} = {}", "rule added:".green().bold(), path, perm_str);

    // Also persist to config.toml
    let config_path = agfs.join("config.toml");
    if config_path.exists() {
        let content = std::fs::read_to_string(&config_path)?;
        let mut doc: toml::Table = content.parse().unwrap_or_default();
        let rules = doc
            .entry("rules")
            .or_insert_with(|| toml::Value::Table(toml::Table::new()));
        if let toml::Value::Table(t) = rules {
            t.insert(path.to_string(), toml::Value::String(perm_str.to_string()));
        }
        std::fs::write(&config_path, doc.to_string())?;
    }

    Ok(())
}

pub fn remove(path: &str) -> Result<()> {
    let resolved = resolve_rule_path(path)?;
    let agfs = ctl::agfs_dir()?;
    let ctl_file = ctl::open_ctl(&agfs)?;

    ctl::ioctl_remove_rule(&ctl_file, &resolved)?;
    eprintln!("{} {}", "rule removed:".yellow().bold(), path);

    // Remove from config.toml
    let config_path = agfs.join("config.toml");
    if config_path.exists() {
        let content = std::fs::read_to_string(&config_path)?;
        let mut doc: toml::Table = content.parse().unwrap_or_default();
        if let Some(toml::Value::Table(t)) = doc.get_mut("rules") {
            t.remove(path);
        }
        std::fs::write(&config_path, doc.to_string())?;
    }

    Ok(())
}

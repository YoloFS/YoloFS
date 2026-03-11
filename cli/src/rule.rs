// agfs CLI — rule.rs
//
// `agfs rule add <path> <perm>` / `agfs rule remove <path>`
//
// Always persists to agfs.toml. If a mount exists, also applies live via ioctl.

use crate::ctl::{self, perm_from_str};
use anyhow::{Context, Result};
use colored::Colorize;
use std::env;

/// Resolve a path: relative paths are resolved against CWD, absolute as-is.
fn resolve_rule_path(path: &str) -> Result<String> {
    if path.starts_with('/') {
        return Ok(path.to_string());
    }
    let cwd = env::current_dir().context("getting cwd")?;
    let full = cwd.join(path);
    Ok(full.to_string_lossy().to_string())
}

/// Return the agfs.toml config path in CWD.
fn config_path() -> Result<std::path::PathBuf> {
    let cwd = env::current_dir().context("getting cwd")?;
    Ok(cwd.join("agfs.toml"))
}

/// Check if an agfs session is mounted.
fn is_mounted() -> bool {
    ctl::agfs_dir().is_ok_and(|d| d.join("mnt").exists())
}

pub fn add(path: &str, perm_str: &str) -> Result<()> {
    let perm = perm_from_str(perm_str)
        .ok_or_else(|| anyhow::anyhow!("unknown permission: {perm_str}"))?;

    // Always persist to agfs.toml
    let cp = config_path()?;
    let mut doc: toml::Table = if cp.exists() {
        std::fs::read_to_string(&cp)?.parse().unwrap_or_default()
    } else {
        toml::Table::new()
    };
    let rules = doc
        .entry("rules")
        .or_insert_with(|| toml::Value::Table(toml::Table::new()));
    if let toml::Value::Table(t) = rules {
        t.insert(path.to_string(), toml::Value::String(perm_str.to_string()));
    }
    std::fs::write(&cp, doc.to_string()).context("writing agfs.toml")?;

    // If mounted, also apply live
    if is_mounted() {
        let resolved = resolve_rule_path(path)?;
        let agfs = ctl::agfs_dir()?;
        let ctl_file = ctl::open_ctl(&agfs)?;
        ctl::ioctl_add_rule(&ctl_file, &resolved, perm)?;
        eprintln!("{} {} = {} {}", "rule added:".green().bold(), path, perm_str, "(live)".green());
    } else {
        eprintln!("{} {} = {}", "rule added:".green().bold(), path, perm_str);
    }

    Ok(())
}

pub fn remove(path: &str) -> Result<()> {
    // Always update agfs.toml
    let cp = config_path()?;
    if cp.exists() {
        let content = std::fs::read_to_string(&cp)?;
        let mut doc: toml::Table = content.parse().unwrap_or_default();
        if let Some(toml::Value::Table(t)) = doc.get_mut("rules") {
            t.remove(path);
        }
        std::fs::write(&cp, doc.to_string()).context("writing agfs.toml")?;
    }

    // If mounted, also apply live
    if is_mounted() {
        let resolved = resolve_rule_path(path)?;
        let agfs = ctl::agfs_dir()?;
        let ctl_file = ctl::open_ctl(&agfs)?;
        ctl::ioctl_remove_rule(&ctl_file, &resolved)?;
        eprintln!("{} {} {}", "rule removed:".yellow().bold(), path, "(live)".yellow());
    } else {
        eprintln!("{} {}", "rule removed:".yellow().bold(), path);
    }

    Ok(())
}

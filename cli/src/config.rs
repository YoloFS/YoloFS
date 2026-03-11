// agfs CLI — config.rs
//
// Manages agfs.toml: init, read, rule add/remove, apply rules on mount.

use crate::ctl::{self, perm_from_str};
use anyhow::{Context, Result};
use colored::Colorize;
use std::env;
use std::fs;
use std::path::Path;

pub const DEFAULT_CONFIG: &str = r#"[mount]
ask_timeout = 0
ask_default = "deny"

[rules]
# System paths — allow by default so commands work without prompts
"/usr" = "allow-rx"
"/lib" = "allow-rx"
"/lib64" = "allow-rx"
"/etc" = "allow-ro"
"/bin" = "allow-rx"
"/sbin" = "allow-rx"
"#;

// ── Config helpers ────────────────────────────────────────────────────

fn config_path() -> Result<std::path::PathBuf> {
    let cwd = env::current_dir().context("getting cwd")?;
    Ok(cwd.join("agfs.toml"))
}

fn read_config(path: &Path) -> Result<toml::Table> {
    let content = fs::read_to_string(path).context("reading agfs.toml")?;
    content.parse().context("parsing agfs.toml")
}

fn write_config(path: &Path, doc: &toml::Table) -> Result<()> {
    fs::write(path, doc.to_string()).context("writing agfs.toml")
}

fn is_mounted() -> bool {
    ctl::agfs_dir().is_ok_and(|d| d.join("mnt").exists())
}

/// Resolve a path through the agfs mount for ioctl.
fn resolve_through_mount(path: &str, mnt: &Path) -> Result<String> {
    let abs = if path.starts_with('/') {
        path.to_string()
    } else {
        let cwd = env::current_dir().context("getting cwd")?;
        cwd.join(path).to_string_lossy().to_string()
    };
    Ok(mnt.join(abs.trim_start_matches('/')).to_string_lossy().to_string())
}

// ── Init ──────────────────────────────────────────────────────────────

pub fn init() -> Result<()> {
    let cp = config_path()?;
    if cp.exists() {
        eprintln!("{}", "agfs.toml already exists".yellow());
        return Ok(());
    }
    fs::write(&cp, DEFAULT_CONFIG).context("writing agfs.toml")?;
    eprintln!("{} {}", "created".green().bold(), cp.display());
    Ok(())
}

// ── Mount options ─────────────────────────────────────────────────────

/// Build kernel mount option string from agfs.toml [mount] section.
pub fn mount_options(agfs_dir: &Path) -> String {
    let storage = agfs_dir.to_string_lossy();
    let mut opts = vec![format!("storage={storage}")];

    let cwd = agfs_dir.parent().unwrap_or(Path::new("."));
    let config_path = cwd.join("agfs.toml");
    if let Ok(doc) = config_path.exists().then(|| read_config(&config_path)).unwrap_or(Err(anyhow::anyhow!(""))) {
        if let Some(toml::Value::Table(m)) = doc.get("mount") {
            if m.get("noperm").and_then(|v| v.as_bool()) == Some(true) {
                opts.push("noperm".to_string());
            }
            if m.get("nostaging").and_then(|v| v.as_bool()) == Some(true) {
                opts.push("nostaging".to_string());
            }
            if let Some(v) = m.get("ask_timeout").and_then(|v| v.as_integer()) {
                opts.push(format!("ask_timeout={v}"));
            }
            if let Some(v) = m.get("ask_default").and_then(|v| v.as_integer()) {
                opts.push(format!("ask_default={v}"));
            }
        }
    }

    opts.join(",")
}

// ── Apply rules from config ───────────────────────────────────────────

/// Read [rules] from agfs.toml and apply via ioctl. Called during mount.
pub fn apply_rules(agfs_dir: &Path) -> Result<()> {
    let cwd = agfs_dir.parent().unwrap_or(Path::new("."));
    let cp = cwd.join("agfs.toml");
    if !cp.exists() {
        return Ok(());
    }

    let doc = read_config(&cp)?;
    let rules = match doc.get("rules") {
        Some(toml::Value::Table(t)) => t,
        _ => return Ok(()),
    };

    let ctl_file = ctl::open_ctl(agfs_dir)?;
    let mnt = agfs_dir.join("mnt");
    let mut count = 0;

    for (path, value) in rules {
        let perm_str = match value.as_str() {
            Some(s) => s,
            None => continue,
        };
        let perm = match perm_from_str(perm_str) {
            Some(p) => p,
            None => {
                eprintln!("agfs: skipping invalid rule: {} = {}", path, perm_str);
                continue;
            }
        };
        let resolved = resolve_through_mount(path, &mnt)?;
        if let Err(e) = ctl::ioctl_add_rule(&ctl_file, &resolved, perm) {
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

// ── Rule add/remove ───────────────────────────────────────────────────

pub fn add_rule(path: &str, perm_str: &str) -> Result<()> {
    let perm = perm_from_str(perm_str)
        .ok_or_else(|| anyhow::anyhow!("unknown permission: {perm_str}"))?;

    // Persist to agfs.toml
    let cp = config_path()?;
    let mut doc: toml::Table = if cp.exists() {
        read_config(&cp)?
    } else {
        toml::Table::new()
    };
    let rules = doc
        .entry("rules")
        .or_insert_with(|| toml::Value::Table(toml::Table::new()));
    if let toml::Value::Table(t) = rules {
        t.insert(path.to_string(), toml::Value::String(perm_str.to_string()));
    }
    write_config(&cp, &doc)?;

    // Apply live if mounted
    if is_mounted() {
        let agfs = ctl::agfs_dir()?;
        let mnt = agfs.join("mnt");
        let resolved = resolve_through_mount(path, &mnt)?;
        let ctl_file = ctl::open_ctl(&agfs)?;
        ctl::ioctl_add_rule(&ctl_file, &resolved, perm)?;
        eprintln!("{} {} = {} {}", "rule added:".green().bold(), path, perm_str, "(live)".green());
    } else {
        eprintln!("{} {} = {}", "rule added:".green().bold(), path, perm_str);
    }

    Ok(())
}

pub fn remove_rule(path: &str) -> Result<()> {
    // Update agfs.toml
    let cp = config_path()?;
    if cp.exists() {
        let mut doc = read_config(&cp)?;
        if let Some(toml::Value::Table(t)) = doc.get_mut("rules") {
            t.remove(path);
        }
        write_config(&cp, &doc)?;
    }

    // Apply live if mounted
    if is_mounted() {
        let agfs = ctl::agfs_dir()?;
        let mnt = agfs.join("mnt");
        let resolved = resolve_through_mount(path, &mnt)?;
        let ctl_file = ctl::open_ctl(&agfs)?;
        ctl::ioctl_remove_rule(&ctl_file, &resolved)?;
        eprintln!("{} {} {}", "rule removed:".yellow().bold(), path, "(live)".yellow());
    } else {
        eprintln!("{} {}", "rule removed:".yellow().bold(), path);
    }

    Ok(())
}

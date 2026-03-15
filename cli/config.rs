// agfs CLI — config.rs
//
// Manages agfs.toml: read, rule add/remove, apply rules on mount.

use crate::ioctl;
use anyhow::{Context, Result};
use colored::Colorize;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::env;
use std::fmt;
use std::fs;
use std::path::Path;
use std::str::FromStr;

// ── Perm enum ────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Perm {
    Ask,
    Allow,
    AllowRw,
    AllowRo,
    AllowRx,
    Deny,
}

impl Perm {
    pub fn to_ioctl(self) -> u8 {
        match self {
            Perm::Ask => ioctl::AGFS_PERM_ASK,
            Perm::Allow => ioctl::AGFS_PERM_ALLOW,
            Perm::AllowRw => ioctl::AGFS_PERM_ALLOW_RW,
            Perm::AllowRo => ioctl::AGFS_PERM_ALLOW_RO,
            Perm::AllowRx => ioctl::AGFS_PERM_ALLOW_RX,
            Perm::Deny => ioctl::AGFS_PERM_DENY,
        }
    }
}

impl fmt::Display for Perm {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Perm::Ask => "ask",
            Perm::Allow => "allow",
            Perm::AllowRw => "allow-rw",
            Perm::AllowRo => "allow-ro",
            Perm::AllowRx => "allow-rx",
            Perm::Deny => "deny",
        })
    }
}

impl FromStr for Perm {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> Result<Self> {
        match s {
            "ask" => Ok(Perm::Ask),
            "allow" => Ok(Perm::Allow),
            "allow-rw" => Ok(Perm::AllowRw),
            "allow-ro" => Ok(Perm::AllowRo),
            "allow-rx" => Ok(Perm::AllowRx),
            "deny" => Ok(Perm::Deny),
            _ => anyhow::bail!("unknown permission: {s}"),
        }
    }
}

// ── Typed config ─────────────────────────────────────────────────────

#[derive(Debug, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub mount: MountConfig,
    #[serde(default)]
    pub exec: ExecConfig,
    #[serde(default)]
    pub rules: BTreeMap<String, Perm>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct MountConfig {
    #[serde(default)]
    pub noperm: bool,
    #[serde(default)]
    pub nostaging: bool,
    #[serde(default)]
    pub ask_timeout: Option<u64>,
    #[serde(default)]
    pub ask_default: Option<Perm>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct ExecConfig {
    #[serde(default)]
    pub auto_snapshot: bool,
}

impl Default for Config {
    fn default() -> Self {
        let rules = BTreeMap::from([
            ("/usr".into(), Perm::AllowRx),
            ("/lib".into(), Perm::AllowRx),
            ("/lib64".into(), Perm::AllowRx),
            ("/etc".into(), Perm::AllowRo),
            ("/bin".into(), Perm::AllowRx),
            ("/sbin".into(), Perm::AllowRx),
        ]);
        Config {
            mount: MountConfig {
                ask_default: Some(Perm::Deny),
                ..Default::default()
            },
            exec: ExecConfig::default(),
            rules,
        }
    }
}

impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        let content = fs::read_to_string(path).context("reading agfs.toml")?;
        toml::from_str(&content).context("parsing agfs.toml")
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        let content = toml::to_string_pretty(self).context("serializing config")?;
        fs::write(path, content).context("writing agfs.toml")
    }
}

// ── Helpers ──────────────────────────────────────────────────────────

fn config_path() -> Result<std::path::PathBuf> {
    let cwd = env::current_dir().context("getting cwd")?;
    Ok(cwd.join("agfs.toml"))
}

fn is_mounted() -> bool {
    crate::utils::session_dir().is_ok_and(|d| d.join("mnt").exists())
}

/// Expand `$HOME`/`~`, then canonicalize (resolves relative paths, symlinks, `..`).
/// Fails if the path doesn't exist — the kernel can only match rules against existing dentries.
fn resolve_to_abs(path: &str) -> Result<String> {
    let expanded = if path.starts_with("$HOME") || path.starts_with('~') {
        let home = env::var("HOME").context("$HOME not set")?;
        if path == "~" || path == "$HOME" {
            home
        } else if let Some(rest) = path.strip_prefix("~/") {
            format!("{home}/{rest}")
        } else if let Some(rest) = path.strip_prefix("$HOME/") {
            format!("{home}/{rest}")
        } else {
            path.to_string()
        }
    } else {
        path.to_string()
    };

    fs::canonicalize(&expanded)
        .map(|p| p.to_string_lossy().to_string())
        .with_context(|| format!("rule path does not exist: {expanded}"))
}

fn resolve_through_mount(abs_path: &str, mnt: &Path) -> String {
    mnt.join(abs_path.trim_start_matches('/'))
        .to_string_lossy()
        .to_string()
}

// ── Mount options ─────────────────────────────────────────────────────

/// Build kernel mount option string from agfs.toml [mount] section.
pub fn mount_options(agfs_dir: &Path) -> String {
    let cwd = agfs_dir.parent().unwrap_or(Path::new("."));
    let config_path = cwd.join("agfs.toml");

    let config = match Config::load(&config_path) {
        Ok(c) => c,
        Err(_) => return String::new(),
    };

    let mut opts = Vec::new();
    let m = &config.mount;

    if m.noperm {
        opts.push("noperm".to_string());
    }
    if m.nostaging {
        opts.push("nostaging".to_string());
    }
    if let Some(v) = m.ask_timeout {
        opts.push(format!("ask_timeout={v}"));
    }
    if let Some(p) = m.ask_default {
        opts.push(format!("ask_default={}", p.to_ioctl()));
    }

    opts.join(",")
}

// ── Apply rules from config ───────────────────────────────────────────

/// Load the exec config section from agfs.toml (if present).
pub fn load_exec_config() -> ExecConfig {
    let cp = match config_path() {
        Ok(p) => p,
        Err(_) => return ExecConfig::default(),
    };
    match Config::load(&cp) {
        Ok(c) => c.exec,
        Err(_) => ExecConfig::default(),
    }
}

/// Read [rules] from agfs.toml and apply via ioctl. Called during mount.
pub fn apply_rules(agfs_dir: &Path) -> Result<()> {
    let cwd = agfs_dir.parent().unwrap_or(Path::new("."));
    let cp = cwd.join("agfs.toml");
    if !cp.exists() {
        return Ok(());
    }

    let config = Config::load(&cp)?;
    if config.rules.is_empty() {
        return Ok(());
    }

    let ctl_file = ioctl::open(agfs_dir)?;
    let mnt = agfs_dir.join("mnt");

    eprintln!(
        "{}",
        format!(
            "agfs: applying {} rule(s) from agfs.toml",
            config.rules.len()
        )
        .cyan()
    );

    for (path, perm) in &config.rules {
        let abs_path = match resolve_to_abs(path) {
            Ok(p) => p,
            Err(e) => {
                eprintln!("  {} {} = {}: {:#}", "✗".red(), path, perm, e);
                continue;
            }
        };
        let resolved = resolve_through_mount(&abs_path, &mnt);
        if let Err(e) = ioctl::add_rule(&ctl_file, &resolved, perm.to_ioctl()) {
            eprintln!("  {} {} = {}: {:#}", "✗".red(), abs_path, perm, e);
        } else {
            eprintln!("  {} {} = {}", "✓".green(), abs_path, perm);
        }
    }

    Ok(())
}

// ── Rule add/remove ───────────────────────────────────────────────────

pub fn add_rule(path: &str, perm_str: &str) -> Result<()> {
    let perm: Perm = perm_str.parse()?;

    // Persist to agfs.toml
    let cp = config_path()?;
    let mut config = if cp.exists() {
        Config::load(&cp)?
    } else {
        Config {
            rules: BTreeMap::new(),
            ..Default::default()
        }
    };
    config.rules.insert(path.to_string(), perm);
    config.save(&cp)?;

    // Apply live if mounted
    if is_mounted() {
        let agfs = crate::utils::session_dir()?;
        let mnt = agfs.join("mnt");
        let abs_path = resolve_to_abs(path)?;
        let resolved = resolve_through_mount(&abs_path, &mnt);
        let ctl_file = ioctl::open(&agfs)?;
        ioctl::add_rule(&ctl_file, &resolved, perm.to_ioctl())?;
        eprintln!(
            "{} {} = {} {}",
            "rule added:".green().bold(),
            path,
            perm,
            "(live)".green()
        );
    } else {
        eprintln!("{} {} = {}", "rule added:".green().bold(), path, perm);
    }

    Ok(())
}

pub fn remove_rule(path: &str) -> Result<()> {
    // Update agfs.toml
    let cp = config_path()?;
    if cp.exists() {
        let mut config = Config::load(&cp)?;
        config.rules.remove(path);
        config.save(&cp)?;
    }

    // Apply live if mounted
    if is_mounted() {
        let agfs = crate::utils::session_dir()?;
        let mnt = agfs.join("mnt");
        let abs_path = resolve_to_abs(path)?;
        let resolved = resolve_through_mount(&abs_path, &mnt);
        let ctl_file = ioctl::open(&agfs)?;
        ioctl::remove_rule(&ctl_file, &resolved)?;
        eprintln!(
            "{} {} {}",
            "rule removed:".yellow().bold(),
            path,
            "(live)".yellow()
        );
    } else {
        eprintln!("{} {}", "rule removed:".yellow().bold(), path);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn perm_display() {
        assert_eq!(Perm::AllowRw.to_string(), "allow-rw");
        assert_eq!(Perm::Deny.to_string(), "deny");
    }

    #[test]
    fn perm_parse() {
        assert_eq!("allow-rw".parse::<Perm>().unwrap(), Perm::AllowRw);
        assert_eq!("deny".parse::<Perm>().unwrap(), Perm::Deny);
        assert!("bogus".parse::<Perm>().is_err());
    }

    #[test]
    fn perm_serde_roundtrip() {
        // TOML requires a table at the top level, so wrap in a struct
        #[derive(Serialize, Deserialize, PartialEq, Debug)]
        struct Wrapper {
            perm: Perm,
        }
        let w = Wrapper {
            perm: Perm::AllowRx,
        };
        let s = toml::to_string(&w).unwrap();
        assert!(s.contains("allow-rx"), "s = {s}");
        let w2: Wrapper = toml::from_str(&s).unwrap();
        assert_eq!(w, w2);
    }

    #[test]
    fn resolve_to_abs_absolute_path() {
        let result = resolve_to_abs("/etc").unwrap();
        assert_eq!(result, "/etc");
    }

    #[test]
    fn resolve_to_abs_nonexistent_fails() {
        assert!(resolve_to_abs("/nonexistent_agfs_test_path").is_err());
    }

    #[test]
    fn resolve_to_abs_home_var() {
        let home = env::var("HOME").unwrap();
        assert_eq!(resolve_to_abs("$HOME").unwrap(), home);
    }

    #[test]
    fn resolve_to_abs_tilde() {
        let home = env::var("HOME").unwrap();
        assert_eq!(resolve_to_abs("~").unwrap(), home);
    }

    #[test]
    fn resolve_through_mount_absolute() {
        let mnt = PathBuf::from("/mnt/agfs");
        let result = resolve_through_mount("/etc/passwd", &mnt);
        assert_eq!(result, "/mnt/agfs/etc/passwd");
    }

    #[test]
    fn resolve_through_mount_strips_leading_slash() {
        let mnt = PathBuf::from("/mnt/agfs");
        let result = resolve_through_mount("/usr/bin", &mnt);
        assert_eq!(result, "/mnt/agfs/usr/bin");
    }

    #[test]
    fn config_default_has_expected_rules() {
        let config = Config::default();
        assert_eq!(config.rules["/usr"], Perm::AllowRx);
        assert_eq!(config.rules["/etc"], Perm::AllowRo);
        assert_eq!(config.mount.ask_default, Some(Perm::Deny));
    }

    #[test]
    fn config_save_load_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("agfs.toml");
        let config = Config::default();
        config.save(&path).unwrap();
        let loaded = Config::load(&path).unwrap();
        assert_eq!(loaded.mount.ask_default, config.mount.ask_default);
        assert_eq!(loaded.rules.len(), config.rules.len());
    }

    #[test]
    fn config_load_missing_file() {
        assert!(Config::load(Path::new("/nonexistent/agfs.toml")).is_err());
    }

    #[test]
    fn config_load_invalid_toml() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("agfs.toml");
        fs::write(&path, "not valid { toml").unwrap();
        assert!(Config::load(&path).is_err());
    }

    #[test]
    fn config_load_empty_sections() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("agfs.toml");
        fs::write(&path, "[mount]\n[rules]\n").unwrap();
        let config = Config::load(&path).unwrap();
        assert!(config.rules.is_empty());
        assert!(!config.mount.noperm);
    }

    #[test]
    fn mount_options_no_config() {
        let tmp = tempfile::tempdir().unwrap();
        let agfs_dir = tmp.path().join(".agfs");
        fs::create_dir_all(&agfs_dir).unwrap();
        let opts = mount_options(&agfs_dir);
        assert_eq!(opts, "");
    }

    #[test]
    fn mount_options_with_flags() {
        let tmp = tempfile::tempdir().unwrap();
        let agfs_dir = tmp.path().join(".agfs");
        fs::create_dir_all(&agfs_dir).unwrap();
        let config = Config {
            mount: MountConfig {
                noperm: true,
                nostaging: true,
                ask_timeout: Some(5),
                ..Default::default()
            },
            exec: Default::default(),
            rules: BTreeMap::new(),
        };
        config.save(&tmp.path().join("agfs.toml")).unwrap();
        let opts = mount_options(&agfs_dir);
        assert!(opts.contains("noperm"), "opts = {opts}");
        assert!(opts.contains("nostaging"), "opts = {opts}");
        assert!(opts.contains("ask_timeout=5"), "opts = {opts}");
    }

    #[test]
    fn mount_options_partial_flags() {
        let tmp = tempfile::tempdir().unwrap();
        let agfs_dir = tmp.path().join(".agfs");
        fs::create_dir_all(&agfs_dir).unwrap();
        let config = Config {
            mount: MountConfig {
                ask_timeout: Some(10),
                ..Default::default()
            },
            exec: Default::default(),
            rules: BTreeMap::new(),
        };
        config.save(&tmp.path().join("agfs.toml")).unwrap();
        let opts = mount_options(&agfs_dir);
        assert!(opts.contains("ask_timeout=10"));
        assert!(!opts.contains("noperm"));
        assert!(!opts.contains("nostaging"));
    }

    #[test]
    fn add_rule_to_config() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("agfs.toml");
        Config {
            mount: Default::default(),
            exec: Default::default(),
            rules: BTreeMap::new(),
        }
        .save(&path)
        .unwrap();

        let mut config = Config::load(&path).unwrap();
        config.rules.insert("/tmp".to_string(), Perm::AllowRw);
        config.save(&path).unwrap();

        let loaded = Config::load(&path).unwrap();
        assert_eq!(loaded.rules["/tmp"], Perm::AllowRw);
    }

    #[test]
    fn remove_rule_from_config() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("agfs.toml");
        let mut config = Config {
            mount: Default::default(),
            exec: Default::default(),
            rules: BTreeMap::new(),
        };
        config.rules.insert("/tmp".to_string(), Perm::AllowRw);
        config.rules.insert("/etc".to_string(), Perm::AllowRo);
        config.save(&path).unwrap();

        let mut loaded = Config::load(&path).unwrap();
        loaded.rules.remove("/tmp");
        loaded.save(&path).unwrap();

        let final_config = Config::load(&path).unwrap();
        assert!(!final_config.rules.contains_key("/tmp"));
        assert!(final_config.rules.contains_key("/etc"));
    }

    #[test]
    fn config_invalid_perm() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("agfs.toml");
        fs::write(&path, "[rules]\n\"/tmp\" = \"bogus\"\n").unwrap();
        assert!(Config::load(&path).is_err());
    }
}

// agfs CLI — config.rs
//
// Manages agfs.toml: init, read, rule add/remove, apply rules on mount.

use crate::ioctl::{self, perm_from_str};
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
    crate::session_dir().is_ok_and(|d| d.join("mnt").exists())
}

/// Resolve a path through the agfs mount for ioctl.
fn resolve_to_abs(path: &str) -> Result<String> {
    if path.starts_with('/') {
        Ok(path.to_string())
    } else {
        let cwd = env::current_dir().context("getting cwd")?;
        Ok(cwd.join(path).to_string_lossy().to_string())
    }
}

fn resolve_through_mount(path: &str, mnt: &Path) -> Result<String> {
    let abs = resolve_to_abs(path)?;
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

    let ctl_file = ioctl::open(agfs_dir)?;
    let mnt = agfs_dir.join("mnt");

    eprintln!("{}", format!("agfs: applying {} rule(s) from agfs.toml", rules.len()).cyan());

    for (path, value) in rules {
        let perm_str = match value.as_str() {
            Some(s) => s,
            None => continue,
        };
        let abs_path = resolve_to_abs(path)?;
        let perm = match perm_from_str(perm_str) {
            Some(p) => p,
            None => {
                eprintln!("  {} {} = {}: invalid permission", "✗".red(), abs_path, perm_str);
                continue;
            }
        };
        let resolved = resolve_through_mount(path, &mnt)?;
        if let Err(e) = ioctl::add_rule(&ctl_file, &resolved, perm) {
            eprintln!("  {} {} = {}: {:#}", "✗".red(), abs_path, perm_str, e);
        } else {
            eprintln!("  {} {} = {}", "✓".green(), abs_path, perm_str);
        }
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
        let agfs = crate::session_dir()?;
        let mnt = agfs.join("mnt");
        let resolved = resolve_through_mount(path, &mnt)?;
        let ctl_file = ioctl::open(&agfs)?;
        ioctl::add_rule(&ctl_file, &resolved, perm)?;
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
        let agfs = crate::session_dir()?;
        let mnt = agfs.join("mnt");
        let resolved = resolve_through_mount(path, &mnt)?;
        let ctl_file = ioctl::open(&agfs)?;
        ioctl::remove_rule(&ctl_file, &resolved)?;
        eprintln!("{} {} {}", "rule removed:".yellow().bold(), path, "(live)".yellow());
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
    fn resolve_to_abs_absolute_path() {
        let result = resolve_to_abs("/etc/passwd").unwrap();
        assert_eq!(result, "/etc/passwd");
    }

    #[test]
    fn resolve_to_abs_relative_path() {
        let result = resolve_to_abs("foo.txt").unwrap();
        let cwd = env::current_dir().unwrap();
        assert_eq!(result, cwd.join("foo.txt").to_string_lossy());
    }

    #[test]
    fn resolve_through_mount_absolute() {
        let mnt = PathBuf::from("/mnt/agfs");
        let result = resolve_through_mount("/etc/passwd", &mnt).unwrap();
        assert_eq!(result, "/mnt/agfs/etc/passwd");
    }

    #[test]
    fn resolve_through_mount_relative() {
        let mnt = PathBuf::from("/mnt/agfs");
        let result = resolve_through_mount("test.txt", &mnt).unwrap();
        let cwd = env::current_dir().unwrap();
        let expected = mnt.join(cwd.strip_prefix("/").unwrap()).join("test.txt");
        assert_eq!(result, expected.to_string_lossy());
    }

    #[test]
    fn mount_options_no_config() {
        let tmp = tempfile::tempdir().unwrap();
        let agfs_dir = tmp.path().join(".agfs");
        fs::create_dir_all(&agfs_dir).unwrap();
        let opts = mount_options(&agfs_dir);
        assert_eq!(opts, format!("storage={}", agfs_dir.to_string_lossy()));
    }

    #[test]
    fn mount_options_with_flags() {
        let tmp = tempfile::tempdir().unwrap();
        let agfs_dir = tmp.path().join(".agfs");
        fs::create_dir_all(&agfs_dir).unwrap();
        fs::write(
            tmp.path().join("agfs.toml"),
            "[mount]\nnoperm = true\nnostaging = true\nask_timeout = 5\n",
        ).unwrap();
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
        fs::write(
            tmp.path().join("agfs.toml"),
            "[mount]\nask_timeout = 10\n",
        ).unwrap();
        let opts = mount_options(&agfs_dir);
        assert!(opts.contains("ask_timeout=10"));
        assert!(!opts.contains("noperm"));
        assert!(!opts.contains("nostaging"));
    }

    #[test]
    fn default_config_is_valid_toml() {
        let doc: toml::Table = DEFAULT_CONFIG.parse().unwrap();
        assert!(doc.contains_key("mount"));
        assert!(doc.contains_key("rules"));
    }

    #[test]
    fn read_write_config_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("agfs.toml");
        fs::write(&path, DEFAULT_CONFIG).unwrap();
        let doc = read_config(&path).unwrap();
        write_config(&path, &doc).unwrap();
        let doc2 = read_config(&path).unwrap();
        assert_eq!(doc, doc2);
    }

    #[test]
    fn read_config_missing_file() {
        assert!(read_config(Path::new("/nonexistent/agfs.toml")).is_err());
    }

    #[test]
    fn read_config_invalid_toml() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("agfs.toml");
        fs::write(&path, "not valid { toml").unwrap();
        assert!(read_config(&path).is_err());
    }

    #[test]
    fn add_rule_persists_to_toml() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("agfs.toml");
        fs::write(&path, "[rules]\n").unwrap();

        // Simulate what add_rule does to the TOML
        let mut doc = read_config(&path).unwrap();
        let rules = doc
            .entry("rules")
            .or_insert_with(|| toml::Value::Table(toml::Table::new()));
        if let toml::Value::Table(t) = rules {
            t.insert("/tmp".to_string(), toml::Value::String("allow-rw".to_string()));
        }
        write_config(&path, &doc).unwrap();

        let doc2 = read_config(&path).unwrap();
        let rules = doc2["rules"].as_table().unwrap();
        assert_eq!(rules["/tmp"].as_str().unwrap(), "allow-rw");
    }

    #[test]
    fn add_rule_invalid_perm() {
        assert!(perm_from_str("bogus").is_none());
    }

    #[test]
    fn remove_rule_from_toml() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("agfs.toml");
        fs::write(&path, "[rules]\n\"/tmp\" = \"allow-rw\"\n\"/etc\" = \"allow-ro\"\n").unwrap();

        let mut doc = read_config(&path).unwrap();
        if let Some(toml::Value::Table(t)) = doc.get_mut("rules") {
            t.remove("/tmp");
        }
        write_config(&path, &doc).unwrap();

        let doc2 = read_config(&path).unwrap();
        let rules = doc2["rules"].as_table().unwrap();
        assert!(!rules.contains_key("/tmp"));
        assert!(rules.contains_key("/etc"));
    }

    #[test]
    fn remove_rule_nonexistent_key() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("agfs.toml");
        fs::write(&path, "[rules]\n\"/etc\" = \"allow-ro\"\n").unwrap();

        let mut doc = read_config(&path).unwrap();
        if let Some(toml::Value::Table(t)) = doc.get_mut("rules") {
            t.remove("/nonexistent"); // no-op
        }
        write_config(&path, &doc).unwrap();

        let doc2 = read_config(&path).unwrap();
        let rules = doc2["rules"].as_table().unwrap();
        assert!(rules.contains_key("/etc"));
    }

    #[test]
    fn add_rule_creates_rules_section() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("agfs.toml");
        fs::write(&path, "[mount]\n").unwrap();

        let mut doc = read_config(&path).unwrap();
        let rules = doc
            .entry("rules")
            .or_insert_with(|| toml::Value::Table(toml::Table::new()));
        if let toml::Value::Table(t) = rules {
            t.insert("/usr".to_string(), toml::Value::String("allow-rx".to_string()));
        }
        write_config(&path, &doc).unwrap();

        let doc2 = read_config(&path).unwrap();
        assert!(doc2.contains_key("mount"));
        let rules = doc2["rules"].as_table().unwrap();
        assert_eq!(rules["/usr"].as_str().unwrap(), "allow-rx");
    }
}

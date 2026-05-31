// yolo CLI — config.rs
//
// Manages yolofs.toml: read, rule add/remove, apply rules on mount.

use crate::ioctl;
use crate::perm::Perm;
use anyhow::{Context, Result};
use colored::Colorize;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::path::Path;

// ── Typed config ─────────────────────────────────────────────────────

fn default_true() -> bool {
    true
}

/// The built-in default config, single source of truth. `yolo init` writes
/// this verbatim (comments and all) and `Config::default()` parses it, so the
/// two never drift. Lives in `example/` alongside the agent-hook templates that
/// `yolo init` also scaffolds.
pub const DEFAULT_CONFIG: &str = include_str!("../example/yolofs.toml");

#[derive(Debug, Serialize, Deserialize)]
pub struct Config {
    #[serde(default = "default_true")]
    pub permission: bool,
    #[serde(default = "default_true")]
    pub staging: bool,
    #[serde(default)]
    pub ask_timeout: Option<u64>,
    #[serde(default)]
    pub ask_default: Option<Perm>,
    #[serde(default)]
    pub snapshot: bool,
    #[serde(default)]
    pub rules: BTreeMap<String, Perm>,
}

impl Default for Config {
    fn default() -> Self {
        // Parse the embedded template so code and `yolo init` share one source.
        // The template ships with the binary, so a parse failure is a build-time
        // bug caught by `config_default_parses` below, never a runtime surprise.
        toml::from_str(DEFAULT_CONFIG).expect("built-in DEFAULT_CONFIG must be valid TOML")
    }
}

impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        let content = fs::read_to_string(path).context("reading yolofs.toml")?;
        toml::from_str(&content).context("parsing yolofs.toml")
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        let content = toml::to_string_pretty(self).context("serializing config")?;
        fs::write(path, content).context("writing yolofs.toml")
    }
}

// ── Helpers ──────────────────────────────────────────────────────────

fn config_path() -> Result<std::path::PathBuf> {
    let cwd = env::current_dir().context("getting cwd")?;
    Ok(cwd.join("yolofs.toml"))
}

fn is_mounted() -> bool {
    crate::utils::session_dir().is_ok_and(|d| d.join("mnt").exists())
}

/// Expand a leading `$HOME` or `~` to the home directory.
fn expand_home(path: &str) -> Result<String> {
    if path == "~" || path == "$HOME" {
        Ok(env::var("HOME").context("$HOME not set")?)
    } else if let Some(rest) = path
        .strip_prefix("~/")
        .or_else(|| path.strip_prefix("$HOME/"))
    {
        Ok(format!(
            "{}/{rest}",
            env::var("HOME").context("$HOME not set")?
        ))
    } else {
        Ok(path.to_string())
    }
}

/// Expand `$HOME`/`~`, then canonicalize (resolves relative paths, symlinks, `..`).
/// Fails if the path doesn't exist — the kernel can only match rules against existing dentries.
fn resolve_to_abs(path: &str) -> Result<String> {
    let expanded = expand_home(path)?;
    fs::canonicalize(&expanded)
        .map(|p| p.to_string_lossy().to_string())
        .with_context(|| format!("rule path does not exist: {expanded}"))
}

/// Lexically normalize a rule/query path to an absolute, `.`/`..`-resolved form
/// *without* requiring it to exist (unlike [`resolve_to_abs`]). Used for rule
/// matching and to display unambiguous paths in `yolo rule` / `rule resolve`.
fn normalize_lexical(path: &str) -> Result<String> {
    let expanded = expand_home(path)?;
    let base = if expanded.starts_with('/') {
        expanded
    } else {
        format!(
            "{}/{expanded}",
            env::current_dir().context("getting cwd")?.to_string_lossy()
        )
    };
    let mut parts: Vec<String> = Vec::new();
    for comp in base.split('/') {
        match comp {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            c => parts.push(c.to_string()),
        }
    }
    Ok(format!("/{}", parts.join("/")))
}

/// True if `ancestor` is `query` itself or a parent directory of it (both
/// already lexically normalized).
fn is_ancestor_or_self(ancestor: &str, query: &str) -> bool {
    ancestor == query
        || ancestor == "/"
        || (query.starts_with(ancestor) && query.as_bytes().get(ancestor.len()) == Some(&b'/'))
}

/// Match `path` against the declared rules: the longest ancestor rule key, plus
/// whether it matches the path exactly (explicit) or via inheritance. `None`
/// means no rule applies (the `ask` default).
fn match_rule<'a>(
    rules: &'a BTreeMap<String, Perm>,
    path: &str,
) -> Result<Option<(&'a str, Perm, bool)>> {
    let query = normalize_lexical(path)?;
    let mut best: Option<(&str, Perm, usize, bool)> = None;
    for (rule_path, perm) in rules {
        let rp = normalize_lexical(rule_path)?;
        if is_ancestor_or_self(&rp, &query) && best.is_none_or(|(_, _, len, _)| rp.len() > len) {
            best = Some((rule_path.as_str(), *perm, rp.len(), rp == query));
        }
    }
    Ok(best.map(|(rp, perm, _, explicit)| (rp, perm, explicit)))
}

fn resolve_through_mount(abs_path: &str, mnt: &Path) -> String {
    mnt.join(abs_path.trim_start_matches('/'))
        .to_string_lossy()
        .to_string()
}

// ── Mount options ─────────────────────────────────────────────────────

/// Build kernel mount option string from yolofs.toml.
pub fn mount_options(yolo_dir: &Path) -> String {
    let cwd = yolo_dir.parent().unwrap_or(Path::new("."));
    let config_path = cwd.join("yolofs.toml");
    let config = Config::load(&config_path).unwrap_or_default();

    let mut opts = vec![
        format!("permission={}", config.permission as u8),
        format!("staging={}", config.staging as u8),
    ];

    if let Some(v) = config.ask_timeout {
        opts.push(format!("ask_timeout={v}"));
    }
    if let Some(p) = config.ask_default {
        opts.push(format!("ask_default={}", p.to_ioctl()));
    }

    opts.join(",")
}

// ── Apply rules from config ───────────────────────────────────────────

/// Read config from yolofs.toml (if present).
pub fn load_config() -> Config {
    let cp = match config_path() {
        Ok(p) => p,
        Err(_) => return Config::default(),
    };
    Config::load(&cp).unwrap_or_default()
}

/// Read [rules] from yolofs.toml and apply via ioctl. Called during mount.
pub fn apply_rules(yolo_dir: &Path) -> Result<()> {
    let cwd = yolo_dir.parent().unwrap_or(Path::new("."));
    let cp = cwd.join("yolofs.toml");
    if !cp.exists() {
        return Ok(());
    }

    let config = Config::load(&cp)?;
    if config.rules.is_empty() {
        return Ok(());
    }

    let ctl_file = ioctl::open(yolo_dir)?;
    let mnt = yolo_dir.join("mnt");

    eprintln!(
        "{}",
        format!(
            "yolo: applying {} rule(s) from yolofs.toml",
            config.rules.len()
        )
        .cyan()
    );

    // Apply silently; only surface rules that fail to apply (a deny that didn't
    // take is worth knowing). Use `yolo rule` to inspect the full set.
    for (path, perm) in &config.rules {
        let abs_path = match resolve_to_abs(path) {
            Ok(p) => p,
            Err(e) => {
                eprintln!("  {} {} = {}: {:#}", "✗".red(), path, perm, e);
                continue;
            }
        };
        let resolved = resolve_through_mount(&abs_path, &mnt);
        if let Err(e) = ioctl::set_rule(&ctl_file, &resolved, perm.to_ioctl()) {
            eprintln!("  {} {} = {}: {:#}", "✗".red(), abs_path, perm, e);
        }
    }

    Ok(())
}

// ── Rule management ───────────────────────────────────────────────────

/// Read yolofs.toml as a format-preserving document, falling back to the
/// built-in template if none exists. Edits via this document keep the file's
/// comments, key order, and layout intact (unlike a serde round-trip).
fn read_doc(cp: &Path) -> Result<toml_edit::DocumentMut> {
    let text = if cp.exists() {
        fs::read_to_string(cp).context("reading yolofs.toml")?
    } else {
        DEFAULT_CONFIG.to_string()
    };
    text.parse::<toml_edit::DocumentMut>()
        .context("parsing yolofs.toml")
}

/// Set `[rules][path] = perm` in place, creating the table if absent.
fn doc_set_rule(doc: &mut toml_edit::DocumentMut, path: &str, perm: Perm) {
    if !doc.contains_key("rules") {
        doc["rules"] = toml_edit::Item::Table(toml_edit::Table::new());
    }
    doc["rules"][path] = toml_edit::value(perm.to_string());
}

/// Remove `[rules][path]` if present.
fn doc_unset_rule(doc: &mut toml_edit::DocumentMut, path: &str) {
    if let Some(rules) = doc.get_mut("rules").and_then(|i| i.as_table_like_mut()) {
        rules.remove(path);
    }
}

/// Set a rule on `path` to `perm`: persist to yolofs.toml and apply live if mounted.
pub fn set_rule(path: &str, perm: Perm) -> Result<()> {
    let cp = config_path()?;
    let mut doc = read_doc(&cp)?;
    doc_set_rule(&mut doc, path, perm);
    fs::write(&cp, doc.to_string()).context("writing yolofs.toml")?;

    if is_mounted() {
        let yolofs = crate::utils::session_dir()?;
        let mnt = yolofs.join("mnt");
        let abs_path = resolve_to_abs(path)?;
        let resolved = resolve_through_mount(&abs_path, &mnt);
        let ctl_file = ioctl::open(&yolofs)?;
        ioctl::set_rule(&ctl_file, &resolved, perm.to_ioctl())?;
        eprintln!(
            "{} {} = {} {}",
            "rule set:".green().bold(),
            path,
            perm,
            "(live)".green()
        );
    } else {
        eprintln!("{} {} = {}", "rule set:".green().bold(), path, perm);
    }
    Ok(())
}

/// Remove any rule on `path`; it reverts to inheriting from its ancestors.
pub fn unset_rule(path: &str) -> Result<()> {
    let cp = config_path()?;
    if cp.exists() {
        let mut doc = read_doc(&cp)?;
        doc_unset_rule(&mut doc, path);
        fs::write(&cp, doc.to_string()).context("writing yolofs.toml")?;
    }

    if is_mounted() {
        let yolofs = crate::utils::session_dir()?;
        let mnt = yolofs.join("mnt");
        let abs_path = resolve_to_abs(path)?;
        let resolved = resolve_through_mount(&abs_path, &mnt);
        let ctl_file = ioctl::open(&yolofs)?;
        ioctl::set_rule(&ctl_file, &resolved, ioctl::YOLO_PERM_UNSET)?;
        eprintln!(
            "{} {} {}",
            "rule unset:".yellow().bold(),
            path,
            "(live)".yellow()
        );
    } else {
        eprintln!("{} {}", "rule unset:".yellow().bold(), path);
    }
    Ok(())
}

/// Print all configured rules (paths sorted), or a notice if there are none.
pub fn list_rules() -> Result<()> {
    let config = load_config();
    if config.rules.is_empty() {
        eprintln!("{}", "no rules configured".dimmed());
        return Ok(());
    }
    // Show each rule's normalized target (`~`/`$HOME` expanded, made absolute,
    // `.`/`..` resolved) so it's unambiguous — but lexically, so symlinks are
    // left alone (`/bin` stays `/bin`). The file keeps the paths as written.
    let mut rows: Vec<(String, Perm)> = config
        .rules
        .iter()
        .map(|(path, perm)| {
            (
                normalize_lexical(path).unwrap_or_else(|_| path.clone()),
                *perm,
            )
        })
        .collect();
    rows.sort_by(|a, b| a.0.cmp(&b.0));
    for (path, perm) in rows {
        println!("{path} = {perm}");
    }
    Ok(())
}

/// Print the effective permission for `path` and where it comes from.
///
/// When mounted, the perm is taken from the kernel (the ground truth it
/// enforces) and the source is annotated from the declared config; a mismatch
/// is surfaced as a warning. When unmounted, it's resolved from the config.
pub fn resolve_rule(path: &str) -> Result<()> {
    let config = load_config();
    let cfg = match_rule(&config.rules, path)?;
    let shown = normalize_lexical(path).unwrap_or_else(|_| path.to_string());
    // Normalize the parent rule a match inherits from, for a consistent display.
    let from = |rp: &str| normalize_lexical(rp).unwrap_or_else(|_| rp.to_string());

    // When mounted, the kernel is authoritative; fall through to the config
    // resolver if the query fails (e.g. the path doesn't exist) or unmounted.
    if is_mounted()
        && let Ok(kperm) = kernel_resolve_perm(path)
    {
        let source = match cfg {
            Some((_, p, true)) if p == kperm => "explicit".to_string(),
            Some((rp, p, false)) if p == kperm => format!("inherited from {}", from(rp)),
            _ => "live".to_string(),
        };
        println!("{shown} → {kperm} ({source})");
        if let Some((_, p, _)) = cfg
            && p != kperm
        {
            eprintln!(
                "{} yolofs.toml says {p}, kernel enforces {kperm}",
                "warning:".yellow().bold()
            );
        }
        return Ok(());
    }

    match cfg {
        Some((_, perm, true)) => println!("{shown} → {perm} (explicit)"),
        Some((rule_path, perm, false)) => {
            println!("{shown} → {perm} (inherited from {})", from(rule_path))
        }
        None => println!("{shown} → {} (default — no matching rule)", Perm::Ask),
    }
    Ok(())
}

/// Ask the kernel for the effective perm it enforces on `path` (authoritative).
fn kernel_resolve_perm(path: &str) -> Result<Perm> {
    let yolofs = crate::utils::session_dir()?;
    let mnt = yolofs.join("mnt");
    let abs_path = resolve_to_abs(path)?;
    let through = resolve_through_mount(&abs_path, &mnt);
    let ctl_file = ioctl::open(&yolofs)?;
    let raw = ioctl::resolve_rule(&ctl_file, &through)?;
    Perm::from_ioctl(raw).context("kernel returned an unknown perm value")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn perm_display() {
        assert_eq!(Perm::Allow.to_string(), "allow");
        assert_eq!(Perm::Read.to_string(), "read");
        assert_eq!(Perm::Hide.to_string(), "hide");
        assert_eq!(Perm::Deny.to_string(), "deny");
    }

    #[test]
    fn perm_parse() {
        assert_eq!("allow".parse::<Perm>().unwrap(), Perm::Allow);
        assert_eq!("read".parse::<Perm>().unwrap(), Perm::Read);
        assert_eq!("hide".parse::<Perm>().unwrap(), Perm::Hide);
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
        let w = Wrapper { perm: Perm::Read };
        let s = toml::to_string(&w).unwrap();
        assert!(s.contains("read"), "s = {s}");
        let w2: Wrapper = toml::from_str(&s).unwrap();
        assert_eq!(w, w2);
    }

    #[test]
    fn match_rule_explicit_inherited_default() {
        // Absolute paths so resolution is independent of cwd.
        let rules = BTreeMap::from([
            ("/etc".to_string(), Perm::Deny),
            ("/etc/hosts".to_string(), Perm::Read),
        ]);

        // Exact match → explicit.
        let (rp, perm, explicit) = match_rule(&rules, "/etc/hosts").unwrap().unwrap();
        assert_eq!((rp, perm, explicit), ("/etc/hosts", Perm::Read, true));

        // Longest ancestor wins, marked inherited.
        let (rp, perm, explicit) = match_rule(&rules, "/etc/passwd").unwrap().unwrap();
        assert_eq!((rp, perm, explicit), ("/etc", Perm::Deny, false));

        // No ancestor rule → default (ask).
        assert!(match_rule(&rules, "/var/log").unwrap().is_none());

        // Prefix-but-not-ancestor must NOT match (`/etc` is not a parent of `/etcfoo`).
        assert!(match_rule(&rules, "/etcfoo").unwrap().is_none());
    }

    #[test]
    fn resolve_to_abs_absolute_path() {
        let result = resolve_to_abs("/etc").unwrap();
        assert_eq!(result, "/etc");
    }

    #[test]
    fn resolve_to_abs_nonexistent_fails() {
        assert!(resolve_to_abs("/nonexistent_yolo_test_path").is_err());
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
        let mnt = PathBuf::from("/mnt/yolofs");
        let result = resolve_through_mount("/etc/passwd", &mnt);
        assert_eq!(result, "/mnt/yolofs/etc/passwd");
    }

    #[test]
    fn resolve_through_mount_strips_leading_slash() {
        let mnt = PathBuf::from("/mnt/yolofs");
        let result = resolve_through_mount("/usr/bin", &mnt);
        assert_eq!(result, "/mnt/yolofs/usr/bin");
    }

    #[test]
    fn config_default_has_expected_rules() {
        let config = Config::default();
        assert_eq!(config.rules["/usr"], Perm::Read);
        assert_eq!(config.rules["/etc"], Perm::Read);
        assert_eq!(config.ask_default, Some(Perm::Deny));
        // Storage is readable from inside the mount (yolofs stacks over /), but
        // the nested mountpoint is hidden to avoid recursion.
        assert_eq!(config.rules[".yolofs"], Perm::Read);
        assert_eq!(config.rules[".yolofs/mnt"], Perm::Hide);
        assert_eq!(config.rules["yolofs.toml"], Perm::Read);
    }

    #[test]
    fn config_default_parses() {
        // Guards the `.expect()` in `Default`: the shipped template must be
        // valid TOML, and what `yolo init` writes must round-trip to the default.
        let parsed: Config = toml::from_str(DEFAULT_CONFIG).expect("template is valid");
        assert_eq!(parsed.rules.len(), Config::default().rules.len());
    }

    #[test]
    fn config_save_load_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("yolofs.toml");
        let config = Config::default();
        config.save(&path).unwrap();
        let loaded = Config::load(&path).unwrap();
        assert_eq!(loaded.ask_default, config.ask_default);
        assert_eq!(loaded.rules.len(), config.rules.len());
    }

    #[test]
    fn config_load_missing_file() {
        assert!(Config::load(Path::new("/nonexistent/yolofs.toml")).is_err());
    }

    #[test]
    fn config_load_invalid_toml() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("yolofs.toml");
        fs::write(&path, "not valid { toml").unwrap();
        assert!(Config::load(&path).is_err());
    }

    #[test]
    fn config_load_empty_sections() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("yolofs.toml");
        fs::write(&path, "[rules]\n").unwrap();
        let config = Config::load(&path).unwrap();
        assert!(config.rules.is_empty());
        assert!(config.permission);
    }

    #[test]
    fn mount_options_no_config() {
        let tmp = tempfile::tempdir().unwrap();
        let yolo_dir = tmp.path().join(".yolofs");
        fs::create_dir_all(&yolo_dir).unwrap();
        let opts = mount_options(&yolo_dir);
        // Falls back to defaults: permission=1,staging=1,ask_default=...
        assert!(opts.contains("permission=1"), "opts = {opts}");
        assert!(opts.contains("staging=1"), "opts = {opts}");
    }

    #[test]
    fn mount_options_with_flags() {
        let tmp = tempfile::tempdir().unwrap();
        let yolo_dir = tmp.path().join(".yolofs");
        fs::create_dir_all(&yolo_dir).unwrap();
        let config = Config {
            permission: false,
            staging: false,
            ask_timeout: Some(5),
            ..Default::default()
        };
        config.save(&tmp.path().join("yolofs.toml")).unwrap();
        let opts = mount_options(&yolo_dir);
        assert!(opts.contains("permission=0"), "opts = {opts}");
        assert!(opts.contains("staging=0"), "opts = {opts}");
        assert!(opts.contains("ask_timeout=5"), "opts = {opts}");
    }

    #[test]
    fn mount_options_partial_flags() {
        let tmp = tempfile::tempdir().unwrap();
        let yolo_dir = tmp.path().join(".yolofs");
        fs::create_dir_all(&yolo_dir).unwrap();
        let config = Config {
            ask_timeout: Some(10),
            ..Default::default()
        };
        config.save(&tmp.path().join("yolofs.toml")).unwrap();
        let opts = mount_options(&yolo_dir);
        assert!(opts.contains("ask_timeout=10"));
        assert!(opts.contains("permission=1"), "opts = {opts}");
        assert!(opts.contains("staging=1"), "opts = {opts}");
    }

    #[test]
    fn add_rule_to_config() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("yolofs.toml");
        Config {
            rules: BTreeMap::new(),
            ..Default::default()
        }
        .save(&path)
        .unwrap();

        let mut config = Config::load(&path).unwrap();
        config.rules.insert("/tmp".to_string(), Perm::Allow);
        config.save(&path).unwrap();

        let loaded = Config::load(&path).unwrap();
        assert_eq!(loaded.rules["/tmp"], Perm::Allow);
    }

    #[test]
    fn remove_rule_from_config() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("yolofs.toml");
        let mut config = Config {
            rules: BTreeMap::new(),
            ..Default::default()
        };
        config.rules.insert("/tmp".to_string(), Perm::Allow);
        config.rules.insert("/etc".to_string(), Perm::Read);
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
        let path = tmp.path().join("yolofs.toml");
        fs::write(&path, "[rules]\n\"/tmp\" = \"bogus\"\n").unwrap();
        assert!(Config::load(&path).is_err());
    }

    #[test]
    fn doc_set_rule_preserves_comments() {
        let mut doc = DEFAULT_CONFIG.parse::<toml_edit::DocumentMut>().unwrap();
        doc_set_rule(&mut doc, "secret.txt", Perm::Deny);
        let out = doc.to_string();
        assert!(out.contains("# System locations"), "comments kept: {out}");
        assert!(out.contains("\"/usr\" = \"read\""), "existing rule kept");
        let cfg: Config = toml::from_str(&out).unwrap();
        assert_eq!(cfg.rules["secret.txt"], Perm::Deny);
    }

    #[test]
    fn doc_unset_rule_removes_key_keeping_other_content() {
        let mut doc = DEFAULT_CONFIG.parse::<toml_edit::DocumentMut>().unwrap();
        doc_unset_rule(&mut doc, "/usr");
        let out = doc.to_string();
        // Unrelated content survives (the header comment, other rules). Note a
        // leading comment owned by the removed key goes with it — that's fine.
        assert!(out.contains("# yolofs.toml"), "header comment kept: {out}");
        let cfg: Config = toml::from_str(&out).unwrap();
        assert!(!cfg.rules.contains_key("/usr"));
        assert!(cfg.rules.contains_key("/bin"), "sibling rules kept");
    }

    #[test]
    fn doc_set_then_unset_restores_file_verbatim() {
        // A rule added and then removed must leave the file byte-for-byte
        // unchanged — this is what lets a demo set rules without dirtying the
        // tracked template.
        let mut doc = DEFAULT_CONFIG.parse::<toml_edit::DocumentMut>().unwrap();
        doc_set_rule(&mut doc, "secret.txt", Perm::Deny);
        doc_unset_rule(&mut doc, "secret.txt");
        assert_eq!(doc.to_string(), DEFAULT_CONFIG);
    }
}

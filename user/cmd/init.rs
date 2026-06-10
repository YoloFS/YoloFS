// yolo CLI — init.rs
//
// `yolo init [path] [--agents <name>...]` — write a default yolofs.toml into
// `path` (the current directory by default, created if missing) and optionally
// scaffold agent pre-tool-use hook templates that wrap shell commands so they
// run through yolofs. The templates are embedded from `user/templates/` at compile time so
// the installed binary works anywhere; `user/templates/` stays the single source of truth.

use crate::config;
use anyhow::{Context, Result};
use clap::ValueEnum;
use colored::Colorize;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

/// The agents `yolo init` can scaffold. As a `ValueEnum` it doubles as the
/// `--agents` parser, so clap validates names and lists them in `--help`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum AgentChoice {
    Claude,
    Gemini,
    Copilot,
}

/// A hook template: where its files go and what they contain.
#[derive(Clone, Copy)]
struct AgentTemplate {
    /// Directory the files land in, under the project root (e.g. `.claude`).
    dir: &'static str,
    /// `(filename, contents)` pairs, written into `dir`.
    files: &'static [(&'static str, &'static str)],
}

impl AgentChoice {
    /// Where this agent's hook files go and what they contain.
    fn template(self) -> AgentTemplate {
        match self {
            AgentChoice::Claude => AgentTemplate {
                dir: ".claude",
                files: &[
                    (
                        "settings.json",
                        include_str!("../templates/.claude/settings.json"),
                    ),
                    ("yolofs.sh", include_str!("../templates/.claude/yolofs.sh")),
                ],
            },
            AgentChoice::Gemini => AgentTemplate {
                dir: ".gemini",
                files: &[
                    (
                        "settings.json",
                        include_str!("../templates/.gemini/settings.json"),
                    ),
                    ("yolofs.sh", include_str!("../templates/.gemini/yolofs.sh")),
                ],
            },
            AgentChoice::Copilot => AgentTemplate {
                dir: ".github/hooks",
                files: &[
                    (
                        "yolofs.json",
                        include_str!("../templates/.github/hooks/yolofs.json"),
                    ),
                    (
                        "yolofs.sh",
                        include_str!("../templates/.github/hooks/yolofs.sh"),
                    ),
                ],
            },
        }
    }
}

/// `yolo init`: write yolofs.toml, then scaffold the selected agents' hooks.
/// An empty selection (no `--agents`) scaffolds every agent.
pub fn run(dir: &Path, agents: &[AgentChoice]) -> Result<()> {
    fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let mut created = 0;
    created += usize::from(write_default_config(dir)?);
    created += scaffold_agents(dir, &resolve_choices(agents))?;
    // Files are written as the invoking user (the CLI carries capabilities, not
    // setuid root, so it never runs as root) — nothing to hand back.
    if created == 0 {
        eprintln!("{} already initialized", "yolo:".cyan());
    }
    Ok(())
}

/// Map choices to templates, deduped and in order. Empty selects every agent.
fn resolve_choices(choices: &[AgentChoice]) -> Vec<AgentTemplate> {
    let chosen = if choices.is_empty() {
        AgentChoice::value_variants().to_vec()
    } else {
        let mut seen = Vec::new();
        for &c in choices {
            if !seen.contains(&c) {
                seen.push(c);
            }
        }
        seen
    };
    chosen.iter().map(|c| c.template()).collect()
}

/// Write the default yolofs.toml (the embedded template, verbatim) unless one
/// already exists. Returns `true` if it was created. Existing files are left
/// untouched and unannounced.
fn write_default_config(dir: &Path) -> Result<bool> {
    let cp = dir.join("yolofs.toml");
    if cp.exists() {
        return Ok(false);
    }
    fs::write(&cp, config::DEFAULT_CONFIG).context("writing yolofs.toml")?;
    eprintln!("{} {}", "created".green().bold(), cp.display());
    Ok(true)
}

/// Write each selected agent's files, creating parent dirs and skipping any
/// file that already exists. `.sh` hooks are made executable. Returns the
/// number of files created; existing files are skipped silently.
fn scaffold_agents(dir: &Path, selected: &[AgentTemplate]) -> Result<usize> {
    let mut created = 0;
    for agent in selected {
        for (name, contents) in agent.files {
            let rel = format!("{}/{}", agent.dir, name);
            let path = dir.join(&rel);
            if path.exists() {
                continue;
            }
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::write(&path, contents)?;
            if name.ends_with(".sh") {
                fs::set_permissions(&path, fs::Permissions::from_mode(0o755))?;
            }
            eprintln!("{} {}", "created".green().bold(), path.display());
            created += 1;
        }
    }
    Ok(created)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_choices_defaults_to_all_and_dedups() {
        use AgentChoice::*;

        // Empty selection scaffolds every agent.
        assert_eq!(
            resolve_choices(&[])
                .iter()
                .map(|t| t.dir)
                .collect::<Vec<_>>(),
            [".claude", ".gemini", ".github/hooks"]
        );

        let two = resolve_choices(&[Claude, Gemini]);
        assert_eq!(
            two.iter().map(|t| t.dir).collect::<Vec<_>>(),
            [".claude", ".gemini"]
        );

        // Duplicates collapse.
        assert_eq!(resolve_choices(&[Claude, Claude]).len(), 1);
    }

    #[test]
    fn write_default_config_creates_file() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(write_default_config(tmp.path()).unwrap(), "reports created");

        let path = tmp.path().join("yolofs.toml");
        assert!(path.exists(), "yolofs.toml should be created");
        let config = config::Config::load(&path).unwrap();
        assert!(config.permission && config.staging);
    }

    #[test]
    fn write_default_config_does_not_overwrite() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("yolofs.toml");
        fs::write(&path, "permission = false\nstaging = false\n").unwrap();

        assert!(
            !write_default_config(tmp.path()).unwrap(),
            "reports not created when one already exists"
        );

        let config = config::Config::load(&path).unwrap();
        assert!(!config.permission, "must not overwrite existing config");
    }

    #[test]
    fn scaffold_writes_files_with_exec_bit_and_is_idempotent() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let claude = AgentChoice::Claude.template();

        assert_eq!(
            scaffold_agents(dir, &[claude]).unwrap(),
            2,
            "two files created"
        );

        let settings = dir.join(".claude/settings.json");
        let hook = dir.join(".claude/yolofs.sh");
        assert!(settings.exists());
        assert!(hook.exists());
        // .sh hook is executable, .json is not forced executable.
        assert_eq!(hook.metadata().unwrap().permissions().mode() & 0o111, 0o111);

        // Unselected agents are untouched.
        assert!(!dir.join(".gemini").exists());

        // Re-running creates nothing and does not clobber an edited file.
        fs::write(&hook, "edited\n").unwrap();
        assert_eq!(
            scaffold_agents(dir, &[claude]).unwrap(),
            0,
            "nothing re-created"
        );
        assert_eq!(fs::read_to_string(&hook).unwrap(), "edited\n");
    }
}

// yolo CLI — init.rs
//
// `yolo init [path] [--agents <name>...]` — write a default yolofs.toml into
// `path` (the current directory by default, created if missing) and optionally
// scaffold agent pre-tool-use hook templates that wrap shell commands so they
// run through yolofs. The templates are embedded from `user/templates/` at compile time so
// the installed binary works anywhere; `user/templates/` stays the single source of truth.

use crate::config;
use crate::report;
use anyhow::{Context, Result};
use clap::ValueEnum;
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

/// The always-loaded agent guide: one canonical source, written under each
/// agent's native context-file name (`CLAUDE.md`/`GEMINI.md`/`AGENTS.md`) so the
/// agent auto-loads it. It explains that commands are staged and lists exactly
/// the subcommands the agent may run (`yolofs::AGENT_ALLOWED`).
const AGENT_GUIDE: &str = include_str!("../templates/agent-guide.md");

/// A scaffold template: the files an agent gets and what they contain.
#[derive(Clone, Copy)]
struct AgentTemplate {
    /// `(path-relative-to-project-root, contents)` pairs. The pre-tool-use hook
    /// lives under a dotdir (e.g. `.claude/`); the guide lives at the root under
    /// the agent's native context-file name.
    files: &'static [(&'static str, &'static str)],
}

impl AgentChoice {
    /// Which files this agent gets and what they contain.
    fn template(self) -> AgentTemplate {
        match self {
            AgentChoice::Claude => AgentTemplate {
                files: &[
                    (
                        ".claude/settings.json",
                        include_str!("../templates/.claude/settings.json"),
                    ),
                    (
                        ".claude/yolofs.sh",
                        include_str!("../templates/.claude/yolofs.sh"),
                    ),
                    ("CLAUDE.md", AGENT_GUIDE),
                ],
            },
            AgentChoice::Gemini => AgentTemplate {
                files: &[
                    (
                        ".gemini/settings.json",
                        include_str!("../templates/.gemini/settings.json"),
                    ),
                    (
                        ".gemini/yolofs.sh",
                        include_str!("../templates/.gemini/yolofs.sh"),
                    ),
                    ("GEMINI.md", AGENT_GUIDE),
                ],
            },
            AgentChoice::Copilot => AgentTemplate {
                files: &[
                    (
                        ".github/hooks/yolofs.json",
                        include_str!("../templates/.github/hooks/yolofs.json"),
                    ),
                    (
                        ".github/hooks/yolofs.sh",
                        include_str!("../templates/.github/hooks/yolofs.sh"),
                    ),
                    ("AGENTS.md", AGENT_GUIDE),
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
        report::hint("already initialized");
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
    report::success(format!("created {}", cp.display()));
    Ok(true)
}

/// Write each selected agent's files, creating parent dirs and skipping any
/// file that already exists. `.sh` hooks are made executable. Returns the
/// number of files created; existing files are skipped silently.
fn scaffold_agents(dir: &Path, selected: &[AgentTemplate]) -> Result<usize> {
    let mut created = 0;
    for agent in selected {
        for (rel, contents) in agent.files {
            let path = dir.join(rel);
            if path.exists() {
                continue;
            }
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::write(&path, contents)?;
            if rel.ends_with(".sh") {
                fs::set_permissions(&path, fs::Permissions::from_mode(0o755))?;
            }
            report::success(format!("created {}", path.display()));
            created += 1;
        }
    }
    Ok(created)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The guide is the last file of each template; its name identifies the agent.
    fn guide_names(templates: &[AgentTemplate]) -> Vec<&'static str> {
        templates
            .iter()
            .map(|t| t.files.last().expect("template has files").0)
            .collect()
    }

    #[test]
    fn resolve_choices_defaults_to_all_and_dedups() {
        use AgentChoice::*;

        // Empty selection scaffolds every agent, each with its native guide file.
        assert_eq!(
            guide_names(&resolve_choices(&[])),
            ["CLAUDE.md", "GEMINI.md", "AGENTS.md"]
        );

        assert_eq!(
            guide_names(&resolve_choices(&[Claude, Gemini])),
            ["CLAUDE.md", "GEMINI.md"]
        );

        // Duplicates collapse.
        assert_eq!(resolve_choices(&[Claude, Claude]).len(), 1);
    }

    #[test]
    fn guide_matches_agent_allowed_command_set() {
        // The scaffolded guide is the agent's contract; it must list exactly the
        // subcommands the CLI gate (`yolofs::AGENT_ALLOWED`) lets the agent run,
        // so the two cannot drift. Human-only commands must not be advertised.
        let allow_line = AGENT_GUIDE
            .lines()
            .find(|l| l.starts_with("You may run only"))
            .expect("guide must state the allow-list on one line");
        for cmd in crate::AGENT_ALLOWED {
            assert!(
                allow_line.contains(&format!("`{cmd}`")),
                "allow-list line must include `{cmd}`"
            );
        }
        for human_only in ["commit", "abort", "rule"] {
            assert!(
                !allow_line.contains(&format!("`{human_only}`")),
                "allow-list line must not advertise human-only `{human_only}`"
            );
        }
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
            3,
            "settings.json + yolofs.sh hook + CLAUDE.md guide"
        );

        let settings = dir.join(".claude/settings.json");
        let hook = dir.join(".claude/yolofs.sh");
        let guide = dir.join("CLAUDE.md");
        assert!(settings.exists());
        assert!(hook.exists());
        // The guide lands at the project root (where Claude auto-loads it), not
        // under the hook dir, and carries the canonical guide content.
        assert_eq!(fs::read_to_string(&guide).unwrap(), AGENT_GUIDE);
        // .sh hook is executable, .json/.md are not forced executable.
        assert_eq!(hook.metadata().unwrap().permissions().mode() & 0o111, 0o111);
        assert_eq!(guide.metadata().unwrap().permissions().mode() & 0o111, 0);

        // Unselected agents are untouched.
        assert!(!dir.join(".gemini").exists());
        assert!(!dir.join("GEMINI.md").exists());

        // Re-running creates nothing and does not clobber an edited file.
        fs::write(&hook, "edited\n").unwrap();
        assert_eq!(
            scaffold_agents(dir, &[claude]).unwrap(),
            0,
            "nothing re-created"
        );
        assert_eq!(fs::read_to_string(&hook).unwrap(), "edited\n");
    }

    #[test]
    fn scaffold_writes_each_agents_native_guide_without_collision() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();

        // All three agents at once: 3 files each, distinct guide filenames.
        let all = resolve_choices(&[]);
        assert_eq!(scaffold_agents(dir, &all).unwrap(), 9, "3 files per agent");

        for guide in ["CLAUDE.md", "GEMINI.md", "AGENTS.md"] {
            let path = dir.join(guide);
            assert_eq!(
                fs::read_to_string(&path).unwrap(),
                AGENT_GUIDE,
                "{guide} carries the canonical guide"
            );
            assert_eq!(
                path.metadata().unwrap().permissions().mode() & 0o111,
                0,
                "{guide} is not executable"
            );
        }
        // Copilot's nested hook dir was created.
        assert!(dir.join(".github/hooks/yolofs.sh").exists());
    }

    #[test]
    fn scaffold_never_overwrites_an_existing_guide() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        // A user already has their own CLAUDE.md — init must leave it intact.
        let existing = dir.join("CLAUDE.md");
        fs::write(&existing, "my own memory\n").unwrap();

        let created = scaffold_agents(dir, &[AgentChoice::Claude.template()]).unwrap();
        assert_eq!(created, 2, "hook files created, existing guide skipped");
        assert_eq!(fs::read_to_string(&existing).unwrap(), "my own memory\n");
    }
}

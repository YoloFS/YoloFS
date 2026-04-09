use crate::workload::{MacroStepSeries, MacroStepTiming, Workload, WorkloadKind};
use anyhow::{Context, Result, bail};
use serde::Deserialize;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Instant;
use yolofs::config::Perm;

#[derive(Deserialize)]
struct SeriesFixture {
    base_commit: String,
    commits: Vec<CommitFixture>,
}

#[derive(Deserialize)]
struct CommitFixture {
    id: String,
    message: String,
    files: Vec<String>,
    search_commands: String,
    read_commands: String,
    edit_commands: String,
}

pub struct MiniDevWorkflow {
    repo_cache: PathBuf,
    fixture_dir: PathBuf,
}

pub fn details() -> crate::workloads::WorkloadDetails {
    crate::workloads::workload_details(
        "Fast macrobenchmark that mirrors dev-workflow with a tiny checked-in Git repo and nested object builds.",
        "Clones the checked-in fixture repo from `bench/fixtures/mini-dev-workflow/source-repo/` into `~/.cache/yolo-bench/mini-dev-workflow` once and reuses checked-in command lists under `bench/fixtures/mini-dev-workflow/`.",
        None,
        "Runs `git worktree add --detach <dest> <base-commit>`, an initial `make`, then per-commit search/read/edit command lists, incremental build, and git status/diff/add/commit with backend-managed checkpoints between phases.",
        file!(),
    )
}

impl MiniDevWorkflow {
    pub fn new() -> Self {
        Self {
            repo_cache: dirs_next::cache_dir()
                .unwrap_or_else(|| PathBuf::from("/tmp"))
                .join("yolo-bench/mini-dev-workflow"),
            fixture_dir: PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("fixtures/mini-dev-workflow"),
        }
    }

    fn source_repo_path(&self) -> PathBuf {
        self.fixture_dir.join("source-repo")
    }

    fn metadata_path(&self) -> PathBuf {
        self.fixture_dir.join("series.json")
    }

    fn load_fixture(&self) -> Result<SeriesFixture> {
        let text = fs::read_to_string(self.metadata_path())
            .context("reading mini-dev-workflow fixture")?;
        serde_json::from_str(&text).context("parsing mini-dev-workflow fixture")
    }

    fn run_cmd(&self, cmd: &mut Command, verbose: bool, what: &str) -> Result<()> {
        cmd.stdout(if verbose {
            Stdio::inherit()
        } else {
            Stdio::null()
        });
        cmd.stderr(if verbose {
            Stdio::inherit()
        } else {
            Stdio::null()
        });
        let status = cmd.status().with_context(|| what.to_string())?;
        if !status.success() {
            bail!("{what} failed");
        }
        Ok(())
    }

    fn run_cmd_timed(&self, cmd: &mut Command, verbose: bool, what: &str) -> Result<u64> {
        let t0 = Instant::now();
        self.run_cmd(cmd, verbose, what)?;
        Ok(t0.elapsed().as_millis() as u64)
    }

    fn load_commands(&self, file_name: &str) -> Result<Vec<String>> {
        let path = self.fixture_dir.join(file_name);
        let text = fs::read_to_string(&path).with_context(|| {
            format!("reading mini-dev-workflow command file {}", path.display())
        })?;
        if text.lines().any(|line| line == "%%") {
            let mut commands = Vec::new();
            let mut current = Vec::new();
            for line in text.lines() {
                if line == "%%" {
                    if !current.is_empty() {
                        commands.push(format!("{}\n", current.join("\n")));
                        current.clear();
                    }
                    continue;
                }
                if current.is_empty() && line.trim_start().starts_with('#') {
                    continue;
                }
                current.push(line.to_owned());
            }
            if !current.is_empty() {
                commands.push(format!("{}\n", current.join("\n")));
            }
            return Ok(commands);
        }
        let mut commands = Vec::new();
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            commands.push(format!("{line}\n"));
        }
        Ok(commands)
    }

    fn run_shell_command(
        &self,
        dest: &Path,
        command: &str,
        verbose: bool,
        what: &str,
    ) -> Result<u64> {
        let mut cmd = Command::new("bash");
        cmd.args(["--noprofile", "--norc", "-c"])
            .arg(command)
            .current_dir(dest)
            .env("LC_ALL", "C");
        self.run_cmd_timed(&mut cmd, verbose, what)
    }

    fn make_cmd(dest: &Path) -> Command {
        let mut cmd = Command::new("make");
        cmd.current_dir(dest);
        cmd
    }
}

impl Workload for MiniDevWorkflow {
    fn name(&self) -> &'static str {
        "mini-dev-workflow"
    }

    fn kind(&self) -> WorkloadKind {
        WorkloadKind::Macro
    }

    fn description(&self) -> &'static str {
        "Tiny git worktree plus search/edit/build/commit replay with nested object outputs"
    }

    fn work_dir(&self) -> &'static str {
        "mini-dev-workflow-dest"
    }

    fn ensure_fixture(&self) -> Result<()> {
        if self.repo_cache.exists() {
            let _ = Command::new("git")
                .args(["worktree", "prune"])
                .current_dir(&self.repo_cache)
                .status();
            return Ok(());
        }
        std::fs::create_dir_all(
            self.repo_cache
                .parent()
                .context("mini-dev-workflow cache parent")?,
        )?;
        let status = Command::new("git")
            .arg("clone")
            .arg(self.source_repo_path())
            .arg(&self.repo_cache)
            .status()
            .context("cloning mini-dev-workflow source repo")?;
        if !status.success() {
            bail!("git clone of mini-dev-workflow fixture failed");
        }
        Ok(())
    }

    fn realistic_rules(&self, session_root: &Path) -> Vec<(String, Perm)> {
        let mut rules = vec![
            (session_root.to_string_lossy().into_owned(), Perm::Allow),
            (self.repo_cache.to_string_lossy().into_owned(), Perm::Allow),
            (
                self.fixture_dir.to_string_lossy().into_owned(),
                Perm::AllowRx,
            ),
            ("/etc".to_string(), Perm::AllowRo),
            ("/etc/gitconfig".to_string(), Perm::Allow),
            ("/tmp".to_string(), Perm::AllowRw),
        ];
        if let Some(home) = dirs_next::home_dir() {
            rules.push((
                home.join(".gitconfig").to_string_lossy().into_owned(),
                Perm::Allow,
            ));
            rules.push((
                home.join(".config/git").to_string_lossy().into_owned(),
                Perm::AllowRx,
            ));
        }
        rules
    }

    fn run(&self, dest: &Path, verbose: bool) -> Result<()> {
        let fixture = self.load_fixture()?;
        let mut checkpoint_step = 0usize;
        let mut macro_steps = Vec::new();
        let dest = if dest == Path::new(".") {
            std::env::current_dir().context("resolving mini-dev-workflow current dir")?
        } else {
            dest.to_path_buf()
        };

        if dest.exists() {
            let mut entries =
                fs::read_dir(&dest).with_context(|| format!("reading {}", dest.display()))?;
            if entries.next().is_some() {
                bail!(
                    "mini-dev-workflow destination is not empty: {}",
                    dest.display()
                );
            }
            fs::remove_dir(&dest)
                .with_context(|| format!("removing placeholder {}", dest.display()))?;
        }

        let prune_t0 = Instant::now();
        let _ = Command::new("git")
            .args(["worktree", "prune"])
            .current_dir(&self.repo_cache)
            .status();
        macro_steps.push(MacroStepTiming {
            step: "worktree: prune".to_string(),
            ms: prune_t0.elapsed().as_millis() as u64,
        });

        let mut add = Command::new("git");
        add.args(["worktree", "add", "--detach"])
            .arg(&dest)
            .arg(&fixture.base_commit)
            .current_dir(&self.repo_cache);
        macro_steps.push(MacroStepTiming {
            step: "worktree: add".to_string(),
            ms: self.run_cmd_timed(&mut add, verbose, "running git worktree add")?,
        });
        if let Some(parent) = dest.parent() {
            std::env::set_current_dir(parent)
                .with_context(|| format!("switching to stable parent {}", parent.display()))?;
        }
        checkpoint_step += 1;
        let cp = super::dev_workflow::emit_checkpoint(checkpoint_step)?;
        macro_steps.push(MacroStepTiming {
            step: "checkpoint: worktree".to_string(),
            ms: cp.checkpoint_ms,
        });
        if cp.stop {
            crate::workloads::emit_macro_step_series(&MacroStepSeries { steps: macro_steps })?;
            return Ok(());
        }

        let mut initial_build = Self::make_cmd(&dest);
        macro_steps.push(MacroStepTiming {
            step: "initial-build: make".to_string(),
            ms: self.run_cmd_timed(&mut initial_build, verbose, "running initial make")?,
        });
        checkpoint_step += 1;
        let cp = super::dev_workflow::emit_checkpoint(checkpoint_step)?;
        macro_steps.push(MacroStepTiming {
            step: "checkpoint: initial-build".to_string(),
            ms: cp.checkpoint_ms,
        });
        if cp.stop {
            crate::workloads::emit_macro_step_series(&MacroStepSeries { steps: macro_steps })?;
            return Ok(());
        }

        for commit in &fixture.commits {
            for (idx, command) in self
                .load_commands(&commit.search_commands)?
                .into_iter()
                .enumerate()
            {
                let ms = self.run_shell_command(
                    &dest,
                    &command,
                    verbose,
                    &format!("running search command for {}", commit.id),
                )?;
                macro_steps.push(MacroStepTiming {
                    step: format!("search: {} #{}", commit.id, idx + 1),
                    ms,
                });
            }
            for (idx, command) in self
                .load_commands(&commit.read_commands)?
                .into_iter()
                .enumerate()
            {
                let ms = self.run_shell_command(
                    &dest,
                    &command,
                    verbose,
                    &format!("running read command for {}", commit.id),
                )?;
                macro_steps.push(MacroStepTiming {
                    step: format!("read: {} #{}", commit.id, idx + 1),
                    ms,
                });
            }
            for (idx, command) in self
                .load_commands(&commit.edit_commands)?
                .into_iter()
                .enumerate()
            {
                let ms = self.run_shell_command(
                    &dest,
                    &command,
                    verbose,
                    &format!("running edit command for {}", commit.id),
                )?;
                macro_steps.push(MacroStepTiming {
                    step: format!("edit: {} #{}", commit.id, idx + 1),
                    ms,
                });
                checkpoint_step += 1;
                let checkpoint = super::dev_workflow::emit_checkpoint(checkpoint_step)?;
                macro_steps.push(MacroStepTiming {
                    step: format!("checkpoint: {} #{}", commit.id, idx + 1),
                    ms: checkpoint.checkpoint_ms,
                });
                if checkpoint.stop {
                    crate::workloads::emit_macro_step_series(&MacroStepSeries {
                        steps: macro_steps,
                    })?;
                    return Ok(());
                }
            }

            let mut incremental_build = Self::make_cmd(&dest);
            macro_steps.push(MacroStepTiming {
                step: format!("incremental-build: {}", commit.id),
                ms: self.run_cmd_timed(
                    &mut incremental_build,
                    verbose,
                    &format!("running incremental make for {}", commit.id),
                )?,
            });
            checkpoint_step += 1;
            let cp = super::dev_workflow::emit_checkpoint(checkpoint_step)?;
            macro_steps.push(MacroStepTiming {
                step: format!("checkpoint: incremental-build {}", commit.id),
                ms: cp.checkpoint_ms,
            });
            if cp.stop {
                crate::workloads::emit_macro_step_series(&MacroStepSeries { steps: macro_steps })?;
                return Ok(());
            }

            let mut status = Command::new("git");
            status.args(["status", "--short"]).current_dir(&dest);
            macro_steps.push(MacroStepTiming {
                step: format!("git-status: {}", commit.id),
                ms: self.run_cmd_timed(&mut status, verbose, "running git status")?,
            });

            let mut diff = Command::new("git");
            diff.args(["diff", "--stat", "--"]).current_dir(&dest);
            for file in &commit.files {
                diff.arg(file);
            }
            macro_steps.push(MacroStepTiming {
                step: format!("git-diff: {}", commit.id),
                ms: self.run_cmd_timed(&mut diff, verbose, "running git diff")?,
            });

            let mut add = Command::new("git");
            add.args(["add", "--"]).current_dir(&dest);
            for file in &commit.files {
                add.arg(file);
            }
            macro_steps.push(MacroStepTiming {
                step: format!("git-add: {}", commit.id),
                ms: self.run_cmd_timed(&mut add, verbose, "running git add")?,
            });

            let mut commit_cmd = Command::new("git");
            commit_cmd
                .args([
                    "-c",
                    "user.name=YoloFS Bench",
                    "-c",
                    "user.email=bench@yolo.local",
                    "commit",
                    "--no-gpg-sign",
                    "-m",
                    &commit.message,
                ])
                .current_dir(&dest);
            macro_steps.push(MacroStepTiming {
                step: format!("git-commit: {}", commit.id),
                ms: self.run_cmd_timed(&mut commit_cmd, verbose, "running git commit")?,
            });
            checkpoint_step += 1;
            let cp = super::dev_workflow::emit_checkpoint(checkpoint_step)?;
            macro_steps.push(MacroStepTiming {
                step: format!("checkpoint: git-commit {}", commit.id),
                ms: cp.checkpoint_ms,
            });
            if cp.stop {
                crate::workloads::emit_macro_step_series(&MacroStepSeries { steps: macro_steps })?;
                return Ok(());
            }
        }

        crate::workloads::emit_macro_step_series(&MacroStepSeries { steps: macro_steps })?;
        Ok(())
    }
}

use crate::workload::{MacroStepSeries, MacroStepTiming, Workload, WorkloadKind};
use agfs::config::Perm;
use anyhow::{bail, Context, Result};
use serde::Deserialize;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Instant;

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

pub struct DevWorkflow {
    linux_fixture: PathBuf,
    fixture_dir: PathBuf,
}

pub fn details() -> crate::workloads::WorkloadDetails {
    crate::workloads::workload_details(
        "Session macrobenchmark that replays a real overlayfs patch series as a search/edit/build/commit workflow on a pinned Linux base commit.",
        "Ensures `~/.cache/agfs-bench/linux` exists as the source repo/object store and reuses checked-in workflow fixtures under `bench/fixtures/dev-workflow/`.",
        None,
        "Runs `git worktree add --detach <dest> <base-commit>`, `make tinyconfig`, a clean build, then per-commit search/read/edit command lists, incremental build, git status/diff/add/commit, and a backend-managed checkpoint after each edit command.",
        file!(),
    )
}

impl DevWorkflow {
    pub fn new() -> Self {
        Self {
            linux_fixture: crate::workloads::worktree::linux_fixture_dir(),
            fixture_dir: PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures/dev-workflow"),
        }
    }

    fn metadata_path(&self) -> PathBuf {
        self.fixture_dir.join("overlayfs-ovl-file.json")
    }

    fn load_fixture(&self) -> Result<SeriesFixture> {
        let text =
            fs::read_to_string(self.metadata_path()).context("reading dev-workflow fixture")?;
        serde_json::from_str(&text).context("parsing dev-workflow fixture")
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
        let text = fs::read_to_string(&path)
            .with_context(|| format!("reading dev-workflow command file {}", path.display()))?;
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

    fn make_cmd(dest: &Path, args: &[&str]) -> Command {
        let mut cmd = Command::new("make");
        cmd.args(args).current_dir(dest);
        cmd
    }
}

impl Workload for DevWorkflow {
    fn name(&self) -> &'static str {
        "dev-workflow"
    }

    fn kind(&self) -> WorkloadKind {
        WorkloadKind::Macro
    }

    fn description(&self) -> &'static str {
        "Pinned Linux worktree plus search/edit/build/commit replay of an overlayfs patch series"
    }

    fn work_dir(&self) -> &'static str {
        "dev-workflow-dest"
    }

    fn ensure_fixture(&self) -> Result<()> {
        crate::workloads::worktree::ensure_linux_fixture(&self.linux_fixture)
    }

    fn realistic_rules(&self, session_root: &Path) -> Vec<(String, Perm)> {
        let mut rules = vec![
            (session_root.to_string_lossy().into_owned(), Perm::Allow),
            (
                self.linux_fixture.to_string_lossy().into_owned(),
                Perm::Allow,
            ),
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
            std::env::current_dir().context("resolving dev-workflow current dir")?
        } else {
            dest.to_path_buf()
        };

        if dest.exists() {
            let mut entries =
                fs::read_dir(&dest).with_context(|| format!("reading {}", dest.display()))?;
            if entries.next().is_some() {
                bail!("dev-workflow destination is not empty: {}", dest.display());
            }
            fs::remove_dir(&dest)
                .with_context(|| format!("removing placeholder {}", dest.display()))?;
        }

        let prune_t0 = Instant::now();
        let _ = Command::new("git")
            .args(["worktree", "prune"])
            .current_dir(&self.linux_fixture)
            .status();
        macro_steps.push(MacroStepTiming {
            step: "worktree: prune".to_string(),
            ms: prune_t0.elapsed().as_millis() as u64,
        });

        let mut add = Command::new("git");
        add.args(["worktree", "add", "--detach"])
            .arg(&dest)
            .arg(&fixture.base_commit)
            .current_dir(&self.linux_fixture);
        macro_steps.push(MacroStepTiming {
            step: "worktree: add".to_string(),
            ms: self.run_cmd_timed(&mut add, verbose, "running git worktree add")?,
        });
        // git worktree add removes and recreates the dest directory.  If
        // `--dest .` was passed (overlayfs backend), our process cwd points at
        // the old (deleted) dentry; re-set it to the freshly-created path so
        // that emit_checkpoint's current_dir() call succeeds later.
        std::env::set_current_dir(&dest)
            .with_context(|| format!("re-entering worktree dir {}", dest.display()))?;
        checkpoint_step += 1;
        let cp = emit_checkpoint(checkpoint_step)?;
        macro_steps.push(MacroStepTiming {
            step: "checkpoint: worktree".to_string(),
            ms: cp.checkpoint_ms,
        });
        if cp.stop {
            crate::workloads::emit_macro_step_series(&MacroStepSeries { steps: macro_steps })?;
            return Ok(());
        }

        let jobs = std::thread::available_parallelism()
            .map(|n| n.get().to_string())
            .unwrap_or_else(|_| "1".to_string());

        let t0 = Instant::now();
        let mut tinyconfig = Self::make_cmd(&dest, &["tinyconfig"]);
        tinyconfig.arg(format!("-j{jobs}"));
        self.run_cmd(&mut tinyconfig, verbose, "running make tinyconfig")?;
        // Enable overlayfs so that incremental builds actually recompile the
        // edited fs/overlayfs/ files.
        let mut enable_ovl = Command::new(dest.join("scripts/config"));
        enable_ovl
            .arg("--enable")
            .arg("OVERLAY_FS")
            .current_dir(&dest);
        self.run_cmd(&mut enable_ovl, verbose, "enabling CONFIG_OVERLAY_FS")?;
        let mut olddefconfig = Self::make_cmd(&dest, &["olddefconfig"]);
        self.run_cmd(&mut olddefconfig, verbose, "running make olddefconfig")?;
        macro_steps.push(MacroStepTiming {
            step: "config: tinyconfig".to_string(),
            ms: t0.elapsed().as_millis() as u64,
        });
        checkpoint_step += 1;
        let cp = emit_checkpoint(checkpoint_step)?;
        macro_steps.push(MacroStepTiming {
            step: "checkpoint: config".to_string(),
            ms: cp.checkpoint_ms,
        });
        if cp.stop {
            crate::workloads::emit_macro_step_series(&MacroStepSeries { steps: macro_steps })?;
            return Ok(());
        }

        let build_arg = format!("-j{jobs}");
        let mut initial_build = Self::make_cmd(&dest, &[&build_arg]);
        macro_steps.push(MacroStepTiming {
            step: "initial-build: make".to_string(),
            ms: self.run_cmd_timed(&mut initial_build, verbose, "running initial make")?,
        });
        checkpoint_step += 1;
        let cp = emit_checkpoint(checkpoint_step)?;
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
                let checkpoint = emit_checkpoint(checkpoint_step)?;
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
            let mut incremental_build = Self::make_cmd(&dest, &[&build_arg]);
            macro_steps.push(MacroStepTiming {
                step: format!("incremental-build: {}", commit.id),
                ms: self.run_cmd_timed(
                    &mut incremental_build,
                    verbose,
                    &format!("running incremental make for {}", commit.id),
                )?,
            });
            checkpoint_step += 1;
            let cp = emit_checkpoint(checkpoint_step)?;
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
                    "user.name=AgFS Bench",
                    "-c",
                    "user.email=bench@agfs.local",
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
            let cp = emit_checkpoint(checkpoint_step)?;
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

#[cfg(test)]
mod tests {
    use super::DevWorkflow;

    #[test]
    fn fixture_commands_are_non_empty() {
        let workload = DevWorkflow::new();
        let fixture = workload.load_fixture().expect("fixture should parse");
        assert!(!fixture.base_commit.is_empty());
        assert!(!fixture.commits.is_empty());
        for commit in fixture.commits {
            assert!(!commit.id.is_empty());
            assert!(!commit.message.is_empty());
            assert!(!workload
                .load_commands(&commit.search_commands)
                .expect("search commands should load")
                .is_empty());
            assert!(!workload
                .load_commands(&commit.read_commands)
                .expect("read commands should load")
                .is_empty());
            assert!(!workload
                .load_commands(&commit.edit_commands)
                .expect("edit commands should load")
                .is_empty());
        }
    }
}

struct CheckpointResponse {
    stop: bool,
    checkpoint_ms: u64,
}

fn emit_checkpoint(step: usize) -> Result<CheckpointResponse> {
    use std::io::{BufRead, Write};

    let saved_cwd = std::env::current_dir()?;
    std::env::set_current_dir("/")?;

    println!("{}", crate::backend::CHECKPOINT_MARKER);
    println!("{{\"step\":{step}}}");
    std::io::stdout().flush()?;

    let stdin = std::io::stdin();
    let mut lock = stdin.lock();
    let mut line = String::new();

    lock.read_line(&mut line)?;
    if line.trim() != crate::backend::RESULTS_MARKER {
        bail!(
            "expected {} after checkpoint",
            crate::backend::RESULTS_MARKER
        );
    }
    line.clear();
    lock.read_line(&mut line)?;

    #[derive(Deserialize)]
    struct Resp {
        stop: bool,
        #[serde(default)]
        checkpoint_ms: u64,
        #[serde(default)]
        next_dest: Option<String>,
    }

    let resp: Resp = serde_json::from_str(line.trim()).context("parsing checkpoint response")?;
    if resp.stop {
        return Ok(CheckpointResponse {
            stop: true,
            checkpoint_ms: resp.checkpoint_ms,
        });
    }
    let target = resp
        .next_dest
        .as_deref()
        .map(Path::new)
        .unwrap_or(&saved_cwd);
    std::env::set_current_dir(target)?;
    Ok(CheckpointResponse {
        stop: false,
        checkpoint_ms: resp.checkpoint_ms,
    })
}

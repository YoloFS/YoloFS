// worktree workload: add a git worktree from a local Linux clone.
//
// Fixture: a regular `git clone` of the Linux kernel (with working tree).
// Run: `git worktree add <dest>` — exercises file creation at scale with no
// pack transfer or delta computation.

use crate::workload::{Workload, WorkloadKind};
use agfs::config::Perm;
use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};
use std::process::Command;

const LINUX_URL: &str = "https://github.com/torvalds/linux.git";

pub struct Worktree {
    fixture: PathBuf,
}

pub fn linux_fixture_dir() -> PathBuf {
    dirs_next::cache_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("agfs-bench/linux")
}

pub fn ensure_linux_fixture(fixture: &Path) -> Result<()> {
    if fixture.exists() {
        // Prune stale worktree registrations left by previous bench runs
        // whose tempdirs have since been deleted.
        let _ = Command::new("git")
            .args(["worktree", "prune"])
            .current_dir(fixture)
            .status();
        return Ok(());
    }
    std::fs::create_dir_all(fixture.parent().unwrap()).context("creating agfs-bench cache dir")?;

    // Prefer cloning from the local mirror if it exists (fast, no network).
    let mirror = fixture.parent().unwrap().join("linux.git");
    let src = if mirror.exists() {
        eprintln!("Cloning linux from local mirror…");
        mirror.to_string_lossy().into_owned()
    } else {
        eprintln!("Cloning linux from {} (this runs once)…", LINUX_URL);
        LINUX_URL.to_string()
    };

    let status = Command::new("git")
        .args(["clone", &src])
        .arg(fixture)
        .status()
        .context("running git clone")?;
    if !status.success() {
        bail!("git clone of linux failed");
    }
    Ok(())
}

pub fn details() -> crate::workloads::WorkloadDetails {
    crate::workloads::workload_details(
        "Session macrobenchmark that materializes a Linux kernel git worktree to simulate large real-world file creation.",
        "Ensures `~/.cache/agfs-bench/linux` exists by cloning Linux once, preferring a local mirror when available, and pruning stale worktree metadata before each run.",
        None,
        "Runs `git worktree add --detach <dest>` inside the cached Linux clone. This creates roughly 80k files with no network transfer.",
        file!(),
    )
}

impl Worktree {
    pub fn new() -> Self {
        Worktree {
            fixture: linux_fixture_dir(),
        }
    }
}

impl Workload for Worktree {
    fn name(&self) -> &'static str {
        "worktree"
    }

    fn kind(&self) -> WorkloadKind {
        WorkloadKind::Macro
    }

    fn description(&self) -> &'static str {
        "git worktree add from a Linux kernel clone (~80k files created)"
    }

    fn work_dir(&self) -> &'static str {
        "worktree-dest"
    }

    fn ensure_fixture(&self) -> Result<()> {
        ensure_linux_fixture(&self.fixture)
    }

    fn realistic_rules(&self, session_root: &Path) -> Vec<(String, Perm)> {
        vec![
            (session_root.to_string_lossy().into_owned(), Perm::AllowRw),
            (self.fixture.to_string_lossy().into_owned(), Perm::Allow),
        ]
    }

    fn run(&self, dest: &Path, verbose: bool) -> Result<()> {
        let _ = Command::new("git")
            .args(["worktree", "prune"])
            .current_dir(&self.fixture)
            .status();

        let status = Command::new("git")
            .args(["worktree", "add", "--detach"])
            .arg(dest)
            .current_dir(&self.fixture)
            .stdout(if verbose {
                std::process::Stdio::inherit()
            } else {
                std::process::Stdio::null()
            })
            .stderr(if verbose {
                std::process::Stdio::inherit()
            } else {
                std::process::Stdio::null()
            })
            .status()
            .context("running git worktree add")?;
        if !status.success() {
            bail!("git worktree add failed");
        }
        Ok(())
    }
}

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

impl Worktree {
    pub fn new() -> Self {
        Worktree {
            fixture: dirs_next::cache_dir()
                .unwrap_or_else(|| PathBuf::from("/tmp"))
                .join("agfs-bench/linux"),
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

    fn work_dir(&self) -> &'static str {
        "worktree-dest"
    }

    fn ensure_fixture(&self) -> Result<()> {
        if self.fixture.exists() {
            // Prune stale worktree registrations left by previous bench runs
            // whose tempdirs have since been deleted.
            let _ = Command::new("git")
                .args(["worktree", "prune"])
                .current_dir(&self.fixture)
                .status();
            return Ok(());
        }
        std::fs::create_dir_all(self.fixture.parent().unwrap())
            .context("creating agfs-bench cache dir")?;

        // Prefer cloning from the local mirror if it exists (fast, no network).
        let mirror = self.fixture.parent().unwrap().join("linux.git");
        let src = if mirror.exists() {
            eprintln!("Cloning linux from local mirror…");
            mirror.to_string_lossy().into_owned()
        } else {
            eprintln!("Cloning linux from {} (this runs once)…", LINUX_URL);
            LINUX_URL.to_string()
        };

        let status = Command::new("git")
            .args(["clone", &src])
            .arg(&self.fixture)
            .status()
            .context("running git clone")?;
        if !status.success() {
            bail!("git clone of linux failed");
        }
        Ok(())
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

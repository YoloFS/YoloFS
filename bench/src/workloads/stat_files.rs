// stat-files workload: stat 1,000 pre-existing files.
//
// Exercises the pure metadata/permission path with no data I/O. For agfs
// this hits agfs_permission and agfs_getattr on every call.

use crate::workload::{Workload, WorkloadKind};
use agfs::config::Perm;
use anyhow::{Context, Result};
use std::path::Path;

pub struct StatFiles;

impl Workload for StatFiles {
    fn name(&self) -> &'static str {
        "stat-files"
    }

    fn kind(&self) -> WorkloadKind {
        WorkloadKind::Micro
    }

    fn description(&self) -> &'static str {
        "Stat 1,000 pre-existing files (pure metadata / permission check overhead)"
    }

    fn work_dir(&self) -> &'static str {
        "stat-dest"
    }

    fn ensure_fixture(&self) -> Result<()> {
        Ok(())
    }

    fn realistic_rules(&self, session_root: &Path) -> Vec<(String, Perm)> {
        vec![(session_root.to_string_lossy().into_owned(), Perm::AllowRw)]
    }

    fn populate_base(&self, base: &Path) -> Result<()> {
        super::populate_files(base, 1000, 4096)
    }

    fn run(&self, dest: &Path, _verbose: bool) -> Result<()> {
        for i in 0..1000 {
            let meta = std::fs::metadata(dest.join(format!("file-{i:04}.dat")))
                .with_context(|| format!("stat file-{i:04}.dat"))?;
            std::hint::black_box(&meta);
        }
        Ok(())
    }
}

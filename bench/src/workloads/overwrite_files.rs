// overwrite-files workload: overwrite 1,000 pre-existing small (4 KiB) files.
//
// Exercises the copy-on-write / copy-up path: for agfs this triggers
// agfs_do_cow on the first write to each file; for overlayfs the kernel
// copies the file from lower to upper before the write lands.

use crate::workload::{Workload, WorkloadKind};
use agfs::config::Perm;
use anyhow::{Context, Result};
use std::path::Path;

pub struct OverwriteFiles;

impl Workload for OverwriteFiles {
    fn name(&self) -> &'static str {
        "overwrite-files"
    }

    fn kind(&self) -> WorkloadKind {
        WorkloadKind::Micro
    }

    fn description(&self) -> &'static str {
        "Overwrite 1,000 pre-existing 4 KiB files (copy-on-write / copy-up path)"
    }

    fn work_dir(&self) -> &'static str {
        "overwrite-dest"
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
        let buf = vec![0xFFu8; 4096];
        for i in 0..1000 {
            std::fs::write(dest.join(format!("file-{i:04}.dat")), &buf)
                .with_context(|| format!("overwriting file-{i:04}.dat"))?;
        }
        Ok(())
    }
}

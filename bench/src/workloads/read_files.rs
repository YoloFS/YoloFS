// read-files workload: read 1,000 pre-existing small (4 KiB) files.
//
// Exercises the read path: for agfs this goes through the lower filesystem
// or reads from a staged inode; for overlayfs it's a passthrough from the
// lower layer (no copy-up on read).

use crate::workload::{Workload, WorkloadKind};
use agfs::config::Perm;
use anyhow::{Context, Result};
use std::path::Path;

pub struct ReadFiles;

impl Workload for ReadFiles {
    fn name(&self) -> &'static str {
        "read-files"
    }

    fn kind(&self) -> WorkloadKind {
        WorkloadKind::Micro
    }

    fn work_dir(&self) -> &'static str {
        "read-dest"
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
            let data = std::fs::read(dest.join(format!("file-{i:04}.dat")))
                .with_context(|| format!("reading file-{i:04}.dat"))?;
            std::hint::black_box(&data);
        }
        Ok(())
    }
}

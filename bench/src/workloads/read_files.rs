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

crate::workloads::define_rust_execution!(
    fn run_read_files(dest: &Path) -> Result<()> {
        for i in 0..1000 {
            let data = std::fs::read(dest.join(format!("file-{i:06}.dat")))
                .with_context(|| format!("reading file-{i:06}.dat"))?;
            std::hint::black_box(&data);
        }
        Ok(())
    } => read_files_execution
);

pub fn details() -> crate::workloads::WorkloadDetails {
    crate::workloads::workload_details(
        "Session microbenchmark for passthrough reads of 1,000 pre-existing 4 KiB files.",
        "Populates the backend base layer with 1,000 files before timing so reads exercise lower-layer lookup instead of creation.",
        None,
        &read_files_execution(),
        file!(),
    )
}

impl Workload for ReadFiles {
    fn name(&self) -> &'static str {
        "read-files"
    }

    fn kind(&self) -> WorkloadKind {
        WorkloadKind::Micro
    }

    fn description(&self) -> &'static str {
        "Read 1,000 pre-existing 4 KiB files (read passthrough / lower fs path)"
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
        run_read_files(dest)
    }
}

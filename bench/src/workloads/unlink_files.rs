// unlink-files workload: delete N pre-existing files.

use crate::workload::{Workload, WorkloadKind};
use yolofs::config::Perm;
use anyhow::{Context, Result};
use std::path::Path;

pub struct UnlinkFiles {
    pub count: usize,
}

fn run_unlink(dest: &Path, count: usize) -> Result<()> {
    for i in 0..count {
        std::fs::remove_file(dest.join(format!("file-{i:06}.dat")))
            .with_context(|| format!("unlinking file-{i:06}.dat"))?;
    }
    Ok(())
}

fn unlink_files_execution() -> String {
    crate::workloads::rust_execution(
        "for i in 0..count {\n\
         \x20   fs::remove_file(dest.join(format!(\"file-{i:06}.dat\")))?;\n\
         }",
    )
}

pub fn details() -> crate::workloads::WorkloadDetails {
    crate::workloads::workload_details(
        "Session microbenchmark for delete-heavy operations on existing files.",
        "Populates the backend base layer with N 4 KiB files before timing.",
        None,
        &unlink_files_execution(),
        file!(),
    )
}

impl Workload for UnlinkFiles {
    fn name(&self) -> &'static str {
        "unlink-files"
    }

    fn kind(&self) -> WorkloadKind {
        WorkloadKind::Micro
    }

    fn description(&self) -> &'static str {
        "Delete 10,000 pre-existing files"
    }

    fn work_dir(&self) -> &'static str {
        "unlink-files"
    }

    fn ensure_fixture(&self) -> Result<()> {
        Ok(())
    }

    fn realistic_rules(&self, session_root: &Path) -> Vec<(String, Perm)> {
        vec![(session_root.to_string_lossy().into_owned(), Perm::AllowRw)]
    }

    fn populate_base(&self, base: &Path) -> Result<()> {
        super::populate_files(base, self.count, 4096)
    }

    fn run(&self, dest: &Path, _verbose: bool) -> Result<()> {
        run_unlink(dest, self.count)
    }
}

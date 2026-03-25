// write-files workload: create N small (4 KiB) files.
//
// Exercises the file-create and sequential-write paths without any network
// dependency; no external fixture is required.
// Parameterized by count for commit-scaling measurements.

use crate::workload::{Workload, WorkloadKind};
use agfs::config::Perm;
use anyhow::{Context, Result};
use std::fs;
use std::path::Path;

pub struct WriteFiles {
    pub count: usize,
}

fn run_write(dest: &Path, count: usize) -> Result<()> {
    fs::create_dir_all(dest).context("creating work dir")?;
    let buf = vec![0u8; 4096];
    for i in 0..count {
        fs::write(dest.join(format!("file-{i:06}.dat")), &buf)
            .with_context(|| format!("writing file-{i:06}.dat"))?;
    }
    Ok(())
}

fn write_files_execution() -> String {
    crate::workloads::rust_execution(
        "let buf = vec![0u8; 4096];\n\
         for i in 0..count {\n\
         \x20   fs::write(dest.join(format!(\"file-{i:06}.dat\")), &buf)?;\n\
         }",
    )
}

pub fn details() -> crate::workloads::WorkloadDetails {
    crate::workloads::workload_details(
        "Session microbenchmark that creates N new 4 KiB files to exercise create + write behavior through each backend.",
        "No external fixture. The workload runs in a fresh work directory created inside the backend session.",
        None,
        &write_files_execution(),
        file!(),
    )
}

impl Workload for WriteFiles {
    fn name(&self) -> &'static str {
        match self.count {
            100 => "write-files-100",
            1000 => "write-files",
            10_000 => "write-files-10k",
            100_000 => "write-files-100k",
            _ => "write-files",
        }
    }

    fn kind(&self) -> WorkloadKind {
        WorkloadKind::Micro
    }

    fn description(&self) -> &'static str {
        match self.count {
            100 => "Create 100 new 4 KiB files",
            1000 => "Create 1,000 new 4 KiB files",
            10_000 => "Create 10,000 new 4 KiB files",
            100_000 => "Create 100,000 new 4 KiB files",
            _ => "Create N new 4 KiB files",
        }
    }

    fn work_dir(&self) -> &'static str {
        self.name()
    }

    fn ensure_fixture(&self) -> Result<()> {
        Ok(())
    }

    fn realistic_rules(&self, session_root: &Path) -> Vec<(String, Perm)> {
        vec![(session_root.to_string_lossy().into_owned(), Perm::AllowRw)]
    }

    fn run(&self, dest: &Path, _verbose: bool) -> Result<()> {
        run_write(dest, self.count)
    }
}

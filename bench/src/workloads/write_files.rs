// write-files workload: create 1,000 small (4 KiB) files.
//
// Exercises the file-create and sequential-write paths without any network
// dependency; no external fixture is required.

use crate::workload::Workload;
use agfs::config::Perm;
use anyhow::{Context, Result};
use std::fs;
use std::path::Path;

pub struct WriteFiles;

impl WriteFiles {
    pub fn new() -> Self {
        WriteFiles
    }
}

impl Workload for WriteFiles {
    fn name(&self) -> &'static str {
        "write-files"
    }

    fn work_dir(&self) -> &'static str {
        "write-dest"
    }

    fn ensure_fixture(&self) -> Result<()> {
        Ok(())
    }

    fn realistic_rules(&self, session_root: &Path) -> Vec<(String, Perm)> {
        vec![(session_root.to_string_lossy().into_owned(), Perm::AllowRw)]
    }

    fn run(&self, dest: &Path, _verbose: bool) -> Result<()> {
        fs::create_dir_all(dest).context("creating work dir")?;
        let buf = vec![0u8; 4096];
        for i in 0..1000 {
            fs::write(dest.join(format!("file-{i:04}.dat")), &buf)
                .with_context(|| format!("writing file-{i:04}.dat"))?;
        }
        Ok(())
    }
}

// write-files workload: write a single small file and commit.
//
// Minimal self-contained workload with no external fixtures. Just enough
// to exercise the create+write+commit path end-to-end.

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
        fs::write(dest.join("hello.txt"), b"hello agfs\n").context("writing hello.txt")
    }
}

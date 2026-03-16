// rename-files workload: rename 1,000 pre-existing files.
//
// Exercises the directory-operation path: for agfs this goes through
// agfs_rename and appends a journal rename record; for overlayfs the
// kernel handles the rename in the upper dir (with copy-up of the parent
// directory).

use crate::workload::{Workload, WorkloadKind};
use agfs::config::Perm;
use anyhow::{Context, Result};
use std::path::Path;

pub struct RenameFiles;

impl Workload for RenameFiles {
    fn name(&self) -> &'static str {
        "rename-files"
    }

    fn kind(&self) -> WorkloadKind {
        WorkloadKind::Micro
    }

    fn work_dir(&self) -> &'static str {
        "rename-dest"
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
            std::fs::rename(
                dest.join(format!("file-{i:04}.dat")),
                dest.join(format!("renamed-{i:04}.dat")),
            )
            .with_context(|| format!("renaming file-{i:04}.dat"))?;
        }
        Ok(())
    }
}

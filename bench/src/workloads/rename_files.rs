// rename-files workload: rename N pre-existing files.
//
// Exercises the directory-operation path: for YoloFS this goes through
// yolo_rename and appends a journal rename record; for overlayfs the
// kernel handles the rename in the upper dir (with copy-up of the parent
// directory).

use crate::workload::{Workload, WorkloadKind};
use yolofs::config::Perm;
use anyhow::{Context, Result};
use std::path::Path;

pub struct RenameFiles {
    pub count: usize,
}

fn run_rename(dest: &Path, count: usize) -> Result<()> {
    for i in 0..count {
        std::fs::rename(
            dest.join(format!("file-{i:06}.dat")),
            dest.join(format!("renamed-{i:06}.dat")),
        )
        .with_context(|| format!("renaming file-{i:06}.dat"))?;
    }
    Ok(())
}

fn rename_files_execution() -> String {
    crate::workloads::rust_execution(
        "for i in 0..count {\n\
         \x20   fs::rename(\n\
         \x20       dest.join(format!(\"file-{i:06}.dat\")),\n\
         \x20       dest.join(format!(\"renamed-{i:06}.dat\")),\n\
         \x20   )?;\n\
         }",
    )
}

pub fn details() -> crate::workloads::WorkloadDetails {
    crate::workloads::workload_details(
        "Session microbenchmark for rename-heavy directory operations on existing files.",
        "Populates the backend base layer with N 4 KiB files before timing.",
        None,
        &rename_files_execution(),
        file!(),
    )
}

impl Workload for RenameFiles {
    fn name(&self) -> &'static str {
        "rename-files"
    }

    fn kind(&self) -> WorkloadKind {
        WorkloadKind::Micro
    }

    fn description(&self) -> &'static str {
        "Rename 10,000 pre-existing files"
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

    fn populate_base(&self, base: &Path) -> Result<()> {
        super::populate_files(base, self.count, 4096)
    }

    fn run(&self, dest: &Path, _verbose: bool) -> Result<()> {
        run_rename(dest, self.count)
    }
}

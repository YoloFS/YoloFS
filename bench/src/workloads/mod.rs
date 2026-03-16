pub mod overwrite_files;
pub mod read_files;
pub mod rename_files;
pub mod stat_files;
pub mod worktree;
pub mod write_files;

use crate::workload::{Workload, WorkloadKind};
use anyhow::{Context, Result};
use std::path::Path;

/// All registered workloads: microbenchmarks first, then macrobenchmarks.
pub fn all() -> Vec<Box<dyn Workload>> {
    vec![
        // micro
        Box::new(write_files::WriteFiles::new()),
        Box::new(read_files::ReadFiles),
        Box::new(stat_files::StatFiles),
        Box::new(overwrite_files::OverwriteFiles),
        Box::new(rename_files::RenameFiles),
        // macro
        Box::new(worktree::Worktree::new()),
    ]
}

pub fn by_name(name: &str) -> Option<Box<dyn Workload>> {
    all().into_iter().find(|w| w.name() == name)
}

pub fn by_kind(kind: WorkloadKind) -> Vec<Box<dyn Workload>> {
    all().into_iter().filter(|w| w.kind() == kind).collect()
}

/// Create `count` files of `size` bytes each in `dir`, named `file-NNNN.dat`.
pub fn populate_files(dir: &Path, count: usize, size: usize) -> Result<()> {
    std::fs::create_dir_all(dir).context("creating base work dir")?;
    let buf = vec![0u8; size];
    for i in 0..count {
        std::fs::write(dir.join(format!("file-{i:04}.dat")), &buf)
            .with_context(|| format!("populating file-{i:04}.dat"))?;
    }
    Ok(())
}

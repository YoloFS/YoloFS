use crate::workload::CacheMode;
use crate::workloads;
use anyhow::{Context, Result, bail};
use std::io::Write;
use std::path::Path;
use std::time::Instant;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum MetaSource {
    Base,
    Stage,
    Checkpoint,
}

pub fn cache_mode(cold: bool) -> CacheMode {
    if cold {
        CacheMode::DropPageCache
    } else {
        CacheMode::Default
    }
}

/// Subdirectory used by cold workloads so that files sit one level deeper
/// than the workload directory. This prevents the agfs mount (which pins
/// the session-root directory inode containing the workload dir entry)
/// from keeping the file listing warm across drop_caches.
pub const COLD_DATA_SUBDIR: &str = "data";

pub fn populate_files_for_source(source: MetaSource, path: &Path) -> Result<()> {
    if source == MetaSource::Base {
        workloads::populate_op_files(path)?;
    }
    Ok(())
}

pub fn populate_files_for_source_cold(source: MetaSource, path: &Path) -> Result<()> {
    if source == MetaSource::Base {
        workloads::populate_op_files(&path.join(COLD_DATA_SUBDIR))?;
    }
    Ok(())
}

pub fn prepare_files_for_source(source: MetaSource, path: &Path) -> Result<()> {
    if matches!(source, MetaSource::Stage | MetaSource::Checkpoint) {
        workloads::populate_op_files(path)?;
    }
    Ok(())
}

pub fn prepare_files_for_source_cold(source: MetaSource, path: &Path) -> Result<()> {
    if matches!(source, MetaSource::Stage | MetaSource::Checkpoint) {
        workloads::populate_op_files(&path.join(COLD_DATA_SUBDIR))?;
    }
    Ok(())
}

pub fn populate_readdir_for_source(source: MetaSource, path: &Path) -> Result<()> {
    if source == MetaSource::Base {
        workloads::populate_readdir_tree(path)?;
    }
    Ok(())
}

pub fn populate_readdir_for_source_cold(source: MetaSource, path: &Path) -> Result<()> {
    if source == MetaSource::Base {
        workloads::populate_readdir_tree(&path.join(COLD_DATA_SUBDIR))?;
    }
    Ok(())
}

pub fn prepare_readdir_for_source(source: MetaSource, path: &Path) -> Result<()> {
    if matches!(source, MetaSource::Stage | MetaSource::Checkpoint) {
        workloads::populate_readdir_tree(path)?;
    }
    Ok(())
}

pub fn prepare_readdir_for_source_cold(source: MetaSource, path: &Path) -> Result<()> {
    if matches!(source, MetaSource::Stage | MetaSource::Checkpoint) {
        workloads::populate_readdir_tree(&path.join(COLD_DATA_SUBDIR))?;
    }
    Ok(())
}

pub fn needs_prepare(source: MetaSource) -> bool {
    matches!(source, MetaSource::Stage | MetaSource::Checkpoint)
}

pub fn needs_checkpoint(source: MetaSource) -> bool {
    source == MetaSource::Checkpoint
}

pub fn run_meta_append(dest: &Path) -> Result<()> {
    let mut latencies = Vec::with_capacity(workloads::OP_FILE_COUNT);
    let buf = vec![0xAB; workloads::OP_FILE_SIZE];
    let total = Instant::now();

    for i in 0..workloads::OP_FILE_COUNT {
        let path = dest.join(format!("file-{i:04}.dat"));
        let t0 = Instant::now();
        std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .with_context(|| format!("opening {}", path.display()))?
            .write_all(&buf)
            .with_context(|| format!("appending {}", path.display()))?;
        latencies.push(t0.elapsed());
    }

    workloads::emit_op_result(&workloads::summarize_latencies(
        latencies,
        total.elapsed(),
        None,
    ))
}

pub fn run_meta_rename(dest: &Path) -> Result<()> {
    let mut latencies = Vec::with_capacity(workloads::OP_FILE_COUNT);
    let total = Instant::now();

    for i in 0..workloads::OP_FILE_COUNT {
        let from = dest.join(format!("file-{i:04}.dat"));
        let to = dest.join(format!("renamed-{i:04}.dat"));
        let t0 = Instant::now();
        std::fs::rename(&from, &to)
            .with_context(|| format!("renaming {} -> {}", from.display(), to.display()))?;
        latencies.push(t0.elapsed());
    }

    workloads::emit_op_result(&workloads::summarize_latencies(
        latencies,
        total.elapsed(),
        None,
    ))
}

pub fn run_meta_unlink(dest: &Path) -> Result<()> {
    let mut latencies = Vec::with_capacity(workloads::OP_FILE_COUNT);
    let total = Instant::now();

    for i in 0..workloads::OP_FILE_COUNT {
        let path = dest.join(format!("file-{i:04}.dat"));
        let t0 = Instant::now();
        std::fs::remove_file(&path).with_context(|| format!("unlinking {}", path.display()))?;
        latencies.push(t0.elapsed());
    }

    workloads::emit_op_result(&workloads::summarize_latencies(
        latencies,
        total.elapsed(),
        None,
    ))
}

pub fn run_meta_stat_cold(dest: &Path) -> Result<()> {
    let path = dest.join(COLD_DATA_SUBDIR).join("file-0000.dat");
    let total = Instant::now();
    let t0 = Instant::now();
    let meta = std::fs::metadata(&path).with_context(|| format!("stat {}", path.display()))?;
    std::hint::black_box(meta);
    let latencies = vec![t0.elapsed()];

    workloads::emit_op_result(&workloads::summarize_latencies(
        latencies,
        total.elapsed(),
        None,
    ))
}

pub fn run_meta_stat_warm(dest: &Path) -> Result<()> {
    workloads::warm_metadata(dest)?;

    let mut latencies = Vec::with_capacity(workloads::OP_FILE_COUNT);
    let total = Instant::now();
    for i in 0..workloads::OP_FILE_COUNT {
        let path = dest.join(format!("file-{i:04}.dat"));
        let t0 = Instant::now();
        let meta = std::fs::metadata(&path).with_context(|| format!("stat {}", path.display()))?;
        std::hint::black_box(meta);
        latencies.push(t0.elapsed());
    }

    workloads::emit_op_result(&workloads::summarize_latencies(
        latencies,
        total.elapsed(),
        None,
    ))
}

pub fn run_meta_readdir_cold(dest: &Path) -> Result<()> {
    let subdir = dest.join(COLD_DATA_SUBDIR).join("dir-0000");
    let total = Instant::now();
    let t0 = Instant::now();
    let mut count = 0usize;
    for entry in
        std::fs::read_dir(&subdir).with_context(|| format!("reading {}", subdir.display()))?
    {
        let entry = entry.with_context(|| format!("iterating {}", subdir.display()))?;
        std::hint::black_box(entry.file_name());
        count += 1;
    }
    if count != workloads::READDIR_FILES_PER_DIR {
        bail!(
            "expected {} entries in {}, found {}",
            workloads::READDIR_FILES_PER_DIR,
            subdir.display(),
            count
        );
    }
    let latencies = vec![t0.elapsed()];

    workloads::emit_op_result(&workloads::summarize_latencies(
        latencies,
        total.elapsed(),
        None,
    ))
}

pub fn run_meta_readdir_warm(dest: &Path) -> Result<()> {
    workloads::warm_readdir_tree(dest)?;

    let mut latencies = Vec::with_capacity(workloads::READDIR_DIR_COUNT);
    let total = Instant::now();
    for d in 0..workloads::READDIR_DIR_COUNT {
        let subdir = dest.join(format!("dir-{d:04}"));
        let t0 = Instant::now();
        let mut count = 0usize;
        for entry in
            std::fs::read_dir(&subdir).with_context(|| format!("reading {}", subdir.display()))?
        {
            let entry = entry.with_context(|| format!("iterating {}", subdir.display()))?;
            std::hint::black_box(entry.file_name());
            count += 1;
        }
        if count != workloads::READDIR_FILES_PER_DIR {
            bail!(
                "expected {} entries in {}, found {}",
                workloads::READDIR_FILES_PER_DIR,
                subdir.display(),
                count
            );
        }
        latencies.push(t0.elapsed());
    }

    workloads::emit_op_result(&workloads::summarize_latencies(
        latencies,
        total.elapsed(),
        None,
    ))
}

pub fn source_fixture(source: MetaSource, noun: &str) -> String {
    match source {
        MetaSource::Base => format!("Populates the {} in the base layer before timing.", noun),
        MetaSource::Stage => {
            format!(
                "Creates the {} inside the mounted staging view before timing.",
                noun
            )
        }
        MetaSource::Checkpoint => {
            format!(
                "Creates the {} inside the mounted staging view, then takes a checkpoint before timing.",
                noun
            )
        }
    }
}

// Backwards-compat alias.
pub fn base_or_stage_fixture(source: MetaSource, noun: &str) -> String {
    source_fixture(source, noun)
}

pub const COLD_MOUNT_CAVEAT: &str = "\
Mounted backends (agfs, overlayfs, branchfs) pin ancestor\n\
directory inodes along the path to the mount point.\n\
These pinned inodes survive drop_caches, giving mounted\n\
backends a head start on cold path resolution compared\n\
to native (which must read every ancestor inode from disk).\n\
This is a real property of VFS mounting, not a benchmark artifact.";

pub fn execution_stub(call: &str) -> String {
    workloads::rust_execution(call)
}

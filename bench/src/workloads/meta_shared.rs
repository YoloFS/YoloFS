use crate::workload::CacheMode;
use crate::workloads;
use anyhow::{Context, Result, bail};
use std::io::Write;
use std::path::Path;
use std::time::{Duration, Instant};

// ── Source axis ──────────────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum MetaSource {
    Base,
    Stage,
    Checkpoint,
}

impl MetaSource {
    pub const ALL: [MetaSource; 3] = [MetaSource::Base, MetaSource::Stage, MetaSource::Checkpoint];
}

// ── Dir size constants ───────────────────────────────────────────────────────

pub const SMALL_DIR: usize = 100;
pub const LARGE_DIR: usize = 10_000;

// ── Shared utilities ─────────────────────────────────────────────────────────

pub fn cache_mode(cold: bool) -> CacheMode {
    if cold {
        CacheMode::DropPageCache
    } else {
        CacheMode::Default
    }
}

/// Subdirectory used by cold workloads so that files sit one level deeper
/// than the workload directory.
pub const COLD_DATA_SUBDIR: &str = "data";

pub fn needs_prepare(source: MetaSource) -> bool {
    matches!(source, MetaSource::Stage | MetaSource::Checkpoint)
}

pub fn needs_checkpoint(source: MetaSource) -> bool {
    source == MetaSource::Checkpoint
}

/// If `workload` ends in `-base`, `-stage`, or `-checkpoint`, return the
/// prefix (the group name). Otherwise `None`.
pub fn source_group_name(workload: &str) -> Option<&str> {
    for suffix in ["-base", "-stage", "-checkpoint"] {
        if let Some(prefix) = workload.strip_suffix(suffix) {
            return Some(prefix);
        }
    }
    None
}

pub const COLD_MOUNT_CAVEAT: &str = "\
Mounted backends (yolo, overlayfs, branchfs) pin ancestor\n\
directory inodes along the path to the mount point.\n\
These pinned inodes survive drop_caches, giving mounted\n\
backends a head start on cold path resolution compared\n\
to native (which must read every ancestor inode from disk).\n\
This is a real property of VFS mounting, not a benchmark artifact.";

// ── Fixture helpers ──────────────────────────────────────────────────────────

pub fn populate_files_for_source(source: MetaSource, path: &Path, count: usize) -> Result<()> {
    if source == MetaSource::Base {
        workloads::populate_op_files(path, count)?;
    }
    Ok(())
}

pub fn populate_files_for_source_cold(source: MetaSource, path: &Path, count: usize) -> Result<()> {
    if source == MetaSource::Base {
        workloads::populate_op_files(&path.join(COLD_DATA_SUBDIR), count)?;
    }
    Ok(())
}

pub fn prepare_files_for_source(source: MetaSource, path: &Path, count: usize) -> Result<()> {
    if matches!(source, MetaSource::Stage | MetaSource::Checkpoint) {
        workloads::populate_op_files(path, count)?;
    }
    Ok(())
}

pub fn prepare_files_for_source_cold(source: MetaSource, path: &Path, count: usize) -> Result<()> {
    if matches!(source, MetaSource::Stage | MetaSource::Checkpoint) {
        workloads::populate_op_files(&path.join(COLD_DATA_SUBDIR), count)?;
    }
    Ok(())
}

// Readdir: single directory with N files.
pub fn populate_readdir_for_source(source: MetaSource, path: &Path, count: usize) -> Result<()> {
    if source == MetaSource::Base {
        workloads::populate_readdir_dir(path, count)?;
    }
    Ok(())
}

pub fn populate_readdir_for_source_cold(
    source: MetaSource,
    path: &Path,
    count: usize,
) -> Result<()> {
    if source == MetaSource::Base {
        workloads::populate_readdir_dir(&path.join(COLD_DATA_SUBDIR), count)?;
    }
    Ok(())
}

pub fn prepare_readdir_for_source(source: MetaSource, path: &Path, count: usize) -> Result<()> {
    if matches!(source, MetaSource::Stage | MetaSource::Checkpoint) {
        workloads::populate_readdir_dir(path, count)?;
    }
    Ok(())
}

pub fn prepare_readdir_for_source_cold(
    source: MetaSource,
    path: &Path,
    count: usize,
) -> Result<()> {
    if matches!(source, MetaSource::Stage | MetaSource::Checkpoint) {
        workloads::populate_readdir_dir(&path.join(COLD_DATA_SUBDIR), count)?;
    }
    Ok(())
}

// ── Core operation functions ─────────────────────────────────────────────────
//
// Each is wrapped in define_rust_execution! so the report can display the
// actual code body. They return Vec<Duration> so the caller can handle
// emit_op_result with any setup it needs.

workloads::define_rust_execution!(
    fn meta_append_core(dest: &Path, count: usize) -> Result<Vec<Duration>> {
        let buf = vec![0xAB; workloads::OP_FILE_SIZE];
        let mut latencies = Vec::with_capacity(count);
        for i in 0..count {
            let path = dest.join(format!("file-{i:06}.dat"));
            let t0 = Instant::now();
            std::fs::OpenOptions::new()
                .append(true)
                .open(&path)
                .with_context(|| format!("opening {}", path.display()))?
                .write_all(&buf)
                .with_context(|| format!("appending {}", path.display()))?;
            latencies.push(t0.elapsed());
        }
        Ok(latencies)
    } => meta_append_core_execution
);

workloads::define_rust_execution!(
    fn meta_rename_core(dest: &Path, count: usize) -> Result<Vec<Duration>> {
        let mut latencies = Vec::with_capacity(count);
        for i in 0..count {
            let from = dest.join(format!("file-{i:06}.dat"));
            let to = dest.join(format!("renamed-{i:06}.dat"));
            let t0 = Instant::now();
            std::fs::rename(&from, &to)
                .with_context(|| format!("renaming {} -> {}", from.display(), to.display()))?;
            latencies.push(t0.elapsed());
        }
        Ok(latencies)
    } => meta_rename_core_execution
);

workloads::define_rust_execution!(
    fn meta_unlink_core(dest: &Path, count: usize) -> Result<Vec<Duration>> {
        let mut latencies = Vec::with_capacity(count);
        for i in 0..count {
            let path = dest.join(format!("file-{i:06}.dat"));
            let t0 = Instant::now();
            std::fs::remove_file(&path).with_context(|| format!("unlinking {}", path.display()))?;
            latencies.push(t0.elapsed());
        }
        Ok(latencies)
    } => meta_unlink_core_execution
);

workloads::define_rust_execution!(
    fn meta_stat_core(dest: &Path, count: usize) -> Result<Vec<Duration>> {
        let mut latencies = Vec::with_capacity(count);
        for i in 0..count {
            let path = dest.join(format!("file-{i:06}.dat"));
            let t0 = Instant::now();
            let meta = std::fs::metadata(&path).with_context(|| format!("stat {}", path.display()))?;
            std::hint::black_box(meta);
            latencies.push(t0.elapsed());
        }
        Ok(latencies)
    } => meta_stat_core_execution
);

workloads::define_rust_execution!(
    fn meta_open_core(dest: &Path, count: usize) -> Result<Vec<Duration>> {
        let mut latencies = Vec::with_capacity(count);
        for i in 0..count {
            let path = dest.join(format!("file-{i:06}.dat"));
            let t0 = Instant::now();
            let f = std::fs::File::open(&path)
                .with_context(|| format!("open {}", path.display()))?;
            std::hint::black_box(f);
            latencies.push(t0.elapsed());
        }
        Ok(latencies)
    } => meta_open_core_execution
);

workloads::define_rust_execution!(
    fn meta_readdir_core(dest: &Path, expected: usize) -> Result<Vec<Duration>> {
        let t0 = Instant::now();
        let mut count = 0usize;
        for entry in
            std::fs::read_dir(dest).with_context(|| format!("reading {}", dest.display()))?
        {
            let entry = entry.with_context(|| format!("iterating {}", dest.display()))?;
            std::hint::black_box(entry.file_name());
            count += 1;
        }
        if count != expected {
            bail!("expected {expected} entries in {}, found {count}", dest.display());
        }
        Ok(vec![t0.elapsed()])
    } => meta_readdir_core_execution
);

// Cold variants of stat and readdir (single operation, cold cache).
workloads::define_rust_execution!(
    fn meta_stat_cold_core(dest: &Path) -> Result<Vec<Duration>> {
        let path = dest.join(COLD_DATA_SUBDIR).join("file-000000.dat");
        let t0 = Instant::now();
        let meta = std::fs::metadata(&path).with_context(|| format!("stat {}", path.display()))?;
        std::hint::black_box(meta);
        Ok(vec![t0.elapsed()])
    } => meta_stat_cold_core_execution
);

workloads::define_rust_execution!(
    fn meta_open_cold_core(dest: &Path) -> Result<Vec<Duration>> {
        let path = dest.join(COLD_DATA_SUBDIR).join("file-000000.dat");
        let t0 = Instant::now();
        let f = std::fs::File::open(&path).with_context(|| format!("open {}", path.display()))?;
        std::hint::black_box(f);
        Ok(vec![t0.elapsed()])
    } => meta_open_cold_core_execution
);

workloads::define_rust_execution!(
    fn meta_readdir_cold_core(dest: &Path, expected: usize) -> Result<Vec<Duration>> {
        let subdir = dest.join(COLD_DATA_SUBDIR);
        let t0 = Instant::now();
        let mut count = 0usize;
        for entry in
            std::fs::read_dir(&subdir).with_context(|| format!("reading {}", subdir.display()))?
        {
            let entry = entry.with_context(|| format!("iterating {}", subdir.display()))?;
            std::hint::black_box(entry.file_name());
            count += 1;
        }
        if count != expected {
            bail!("expected {expected} entries in {}, found {count}", subdir.display());
        }
        Ok(vec![t0.elapsed()])
    } => meta_readdir_cold_core_execution
);

// ── Convenience runners ──────────────────────────────────────────────────────
//
// Each calls the core function, wraps in timing, and emits the OpResult.

pub fn run_meta_append(dest: &Path, count: usize) -> Result<()> {
    let total = Instant::now();
    let latencies = meta_append_core(dest, count)?;
    workloads::emit_op_result(&workloads::summarize_latencies(
        latencies,
        total.elapsed(),
        None,
    ))
}

pub fn run_meta_rename(dest: &Path, count: usize) -> Result<()> {
    let total = Instant::now();
    let latencies = meta_rename_core(dest, count)?;
    workloads::emit_op_result(&workloads::summarize_latencies(
        latencies,
        total.elapsed(),
        None,
    ))
}

pub fn run_meta_unlink(dest: &Path, count: usize) -> Result<()> {
    let total = Instant::now();
    let latencies = meta_unlink_core(dest, count)?;
    workloads::emit_op_result(&workloads::summarize_latencies(
        latencies,
        total.elapsed(),
        None,
    ))
}

pub fn run_meta_stat(dest: &Path, count: usize) -> Result<()> {
    workloads::warm_metadata(dest, count)?;
    let total = Instant::now();
    let latencies = meta_stat_core(dest, count)?;
    workloads::emit_op_result(&workloads::summarize_latencies(
        latencies,
        total.elapsed(),
        None,
    ))
}

pub fn run_meta_open(dest: &Path, count: usize) -> Result<()> {
    workloads::warm_metadata(dest, count)?;
    let total = Instant::now();
    let latencies = meta_open_core(dest, count)?;
    workloads::emit_op_result(&workloads::summarize_latencies(
        latencies,
        total.elapsed(),
        None,
    ))
}

pub fn run_meta_readdir(dest: &Path, count: usize) -> Result<()> {
    workloads::warm_readdir_dir(dest, count)?;
    let total = Instant::now();
    let latencies = meta_readdir_core(dest, count)?;
    workloads::emit_op_result(&workloads::summarize_latencies(
        latencies,
        total.elapsed(),
        None,
    ))
}

pub fn run_meta_stat_cold(dest: &Path) -> Result<()> {
    let total = Instant::now();
    let latencies = meta_stat_cold_core(dest)?;
    workloads::emit_op_result(&workloads::summarize_latencies(
        latencies,
        total.elapsed(),
        None,
    ))
}

pub fn run_meta_open_cold(dest: &Path) -> Result<()> {
    let total = Instant::now();
    let latencies = meta_open_cold_core(dest)?;
    workloads::emit_op_result(&workloads::summarize_latencies(
        latencies,
        total.elapsed(),
        None,
    ))
}

pub fn run_meta_readdir_cold(dest: &Path, count: usize) -> Result<()> {
    let total = Instant::now();
    let latencies = meta_readdir_cold_core(dest, count)?;
    workloads::emit_op_result(&workloads::summarize_latencies(
        latencies,
        total.elapsed(),
        None,
    ))
}

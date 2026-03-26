pub mod checkpoint_scaling;
pub mod fio_rand_read_cold;
pub mod fio_rand_read_warm;
pub mod fio_rand_write;
pub mod fio_randrw_cold;
pub mod fio_randrw_warm;
pub mod fio_seq_read_cold;
pub mod fio_seq_read_warm;
pub mod fio_seq_write;
pub mod linux_untar;
pub mod meta_append;
pub mod meta_create;
pub mod meta_open_cold;
pub mod meta_open_warm;
pub mod meta_readdir_cold;
pub mod meta_readdir_warm;
pub mod meta_rename;
pub mod meta_shared;
pub mod meta_stat_cold;
pub mod meta_stat_warm;
pub mod meta_unlink;
pub mod overwrite_files;
pub mod rename_files;
pub mod unlink_files;
pub mod worktree;
pub mod write_files;

use crate::backend;
use crate::workload::OpResult;
use crate::workload::{Workload, WorkloadKind};
use agfs::config::Perm;
use anyhow::{Context, Result, bail};
use serde_json::Value;
use std::fs::File;
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

pub const OP_FILE_SIZE: usize = 4096;
pub const FIO_FILE_SIZE: &str = "1g";
pub const FIO_IO_SIZE: &str = "256m";
pub const FIO_FILE_NAME: &str = "testfile.dat";

#[derive(Clone, Copy)]
pub struct FioSpec {
    pub name: &'static str,
    pub rw: &'static str,
    pub warm_cache: bool,
    pub seed_existing_file: bool,
    pub mix_read_percent: Option<u8>,
    /// Override io_size (default: FIO_IO_SIZE = "256m").
    pub io_size: Option<&'static str>,
}

pub struct WorkloadDetails {
    pub summary: String,
    pub fixture: String,
    pub harness: Option<String>,
    pub execution: String,
    pub source_path: String,
    /// Measurement caveat shown as a footnote and bar hover text in the report.
    pub caveat: Option<String>,
}

macro_rules! define_rust_execution {
    (fn $name:ident($($arg:ident : $arg_ty:ty),* $(,)?) -> $ret:ty $body:block => $render_name:ident) => {
        pub(crate) fn $name($($arg: $arg_ty),*) -> $ret $body

        pub(crate) fn $render_name() -> String {
            crate::workloads::rust_execution(stringify!($body))
        }
    };
}

pub(crate) use define_rust_execution;

/// All registered workloads: microbenchmarks first, then macrobenchmarks.
pub fn all() -> Vec<Box<dyn Workload>> {
    let mut v: Vec<Box<dyn Workload>> = vec![
        // micro (write-related, 10K files each)
        Box::new(write_files::WriteFiles { count: 10_000 }),
        Box::new(overwrite_files::OverwriteFiles { count: 10_000 }),
        Box::new(rename_files::RenameFiles { count: 10_000 }),
        Box::new(unlink_files::UnlinkFiles { count: 10_000 }),
    ];
    // hidden (used by subcommands only)
    v.push(Box::new(checkpoint_scaling::CheckpointScaling));
    // macro
    v.push(Box::new(worktree::Worktree::new()));
    v.push(Box::new(linux_untar::LinuxUntar::new()));
    // op
    v.push(Box::new(fio_seq_read_cold::FioSeqReadCold));
    v.push(Box::new(fio_seq_read_warm::FioSeqReadWarm));
    v.push(Box::new(fio_seq_write::FioSeqWrite));
    v.push(Box::new(fio_rand_read_cold::FioRandReadCold));
    v.push(Box::new(fio_rand_read_warm::FioRandReadWarm));
    v.push(Box::new(fio_rand_write::FioRandWrite));
    v.push(Box::new(fio_randrw_cold::FioRandRwCold));
    v.push(Box::new(fio_randrw_warm::FioRandRwWarm));
    v.push(Box::new(meta_create::MetaCreate {
        count: meta_shared::LARGE_DIR,
    }));
    v.push(Box::new(meta_create::MetaCreate {
        count: meta_shared::SMALL_DIR,
    }));
    v.push(Box::new(meta_create::MetaCreate { count: 100_000 }));
    for w in meta_append::MetaAppend::all() {
        v.push(Box::new(w));
    }
    for w in meta_open_cold::MetaOpenCold::all() {
        v.push(Box::new(w));
    }
    for w in meta_open_warm::MetaOpenWarm::all() {
        v.push(Box::new(w));
    }
    for w in meta_stat_cold::MetaStatCold::all() {
        v.push(Box::new(w));
    }
    for w in meta_stat_warm::MetaStatWarm::all() {
        v.push(Box::new(w));
    }
    for w in meta_readdir_cold::MetaReaddirCold::all() {
        v.push(Box::new(w));
    }
    for w in meta_readdir_warm::MetaReaddirWarm::all() {
        v.push(Box::new(w));
    }
    for w in meta_rename::MetaRename::all() {
        v.push(Box::new(w));
    }
    for w in meta_unlink::MetaUnlink::all() {
        v.push(Box::new(w));
    }
    v
}

pub fn by_name(name: &str) -> Option<Box<dyn Workload>> {
    all().into_iter().find(|w| w.name() == name)
}

/// Expand a workload selector into concrete workload instances.
///
/// - If `name` is an exact workload name, returns that one workload.
/// - Otherwise, if `name` matches a source-variant group (e.g. `meta-append`),
///   returns all source variants in canonical registration order.
pub fn expand_selector(name: &str) -> Vec<Box<dyn Workload>> {
    if let Some(w) = by_name(name) {
        return vec![w];
    }
    all()
        .into_iter()
        .filter(|w| meta_shared::source_group_name(w.name()) == Some(name))
        .collect()
}

/// Source-variant group names in canonical registration order.
pub fn source_groups() -> Vec<&'static str> {
    let mut groups = Vec::new();
    for w in all() {
        if let Some(group) = meta_shared::source_group_name(w.name())
            && !groups.contains(&group)
        {
            groups.push(group);
        }
    }
    groups
}

pub fn by_kind(kind: WorkloadKind) -> Vec<Box<dyn Workload>> {
    all().into_iter().filter(|w| w.kind() == kind).collect()
}

/// Map of workload name → description (for report tooltips).
pub fn descriptions() -> std::collections::HashMap<&'static str, &'static str> {
    all().iter().map(|w| (w.name(), w.description())).collect()
}

pub fn details(name: &str) -> Option<WorkloadDetails> {
    // Strip source suffix to get the group name for meta workloads.
    let group = meta_shared::source_group_name(name).unwrap_or(name);
    Some(match group {
        "write-files" => write_files::details(),
        "overwrite-files" => overwrite_files::details(),
        "rename-files" => rename_files::details(),
        "unlink-files" => unlink_files::details(),
        "checkpoint-scaling" => checkpoint_scaling::details(),
        "worktree" => worktree::details(),
        "linux-untar" => linux_untar::details(),
        "meta-create" | "meta-create-100" | "meta-create-100k" => meta_create::details(),
        "meta-append" | "meta-append-100" => meta_append::details(),
        "meta-open-cold" => meta_open_cold::details(),
        "meta-open" | "meta-open-100" => meta_open_warm::details(),
        "meta-stat-cold" => meta_stat_cold::details(),
        "meta-stat" | "meta-stat-100" => meta_stat_warm::details(),
        "meta-readdir-cold" => meta_readdir_cold::details(),
        "meta-readdir" | "meta-readdir-100" => meta_readdir_warm::details(),
        "meta-rename" | "meta-rename-100" => meta_rename::details(),
        "meta-unlink" | "meta-unlink-100" => meta_unlink::details(),
        "fio-seq-read-cold" => fio_seq_read_cold::details(),
        "fio-seq-read-warm" => fio_seq_read_warm::details(),
        "fio-seq-write" => fio_seq_write::details(),
        "fio-rand-read-cold" => fio_rand_read_cold::details(),
        "fio-rand-read-warm" => fio_rand_read_warm::details(),
        "fio-rand-write" => fio_rand_write::details(),
        "fio-randrw-cold" => fio_randrw_cold::details(),
        "fio-randrw-warm" => fio_randrw_warm::details(),
        _ => return None,
    })
}

/// Return the caveat string for a workload, if any.
pub fn caveat(name: &str) -> Option<String> {
    details(name).and_then(|d| d.caveat)
}

pub fn workload_details(
    summary: &str,
    fixture: &str,
    harness: Option<&str>,
    execution: &str,
    source_path: &str,
) -> WorkloadDetails {
    WorkloadDetails {
        summary: summary.to_string(),
        fixture: fixture.to_string(),
        harness: harness.map(str::to_string),
        execution: execution.to_string(),
        source_path: source_path.to_string(),
        caveat: None,
    }
}

pub fn fio_workload_details(summary: &str, source_path: &str, spec: FioSpec) -> WorkloadDetails {
    let file_path = Path::new("<dest>").join(FIO_FILE_NAME);
    let jobfile_path = Path::new("<dest>").join("job.fio");
    WorkloadDetails {
        summary: summary.to_string(),
        fixture: if spec.seed_existing_file {
            "The workload creates `<dest>/testfile.dat` inside the mounted sandbox as a 1 GiB deterministic patterned file before running fio.".to_string()
        } else {
            "No pre-existing file is required; fio creates `<dest>/testfile.dat` inside the workload directory.".to_string()
        },
        harness: Some(if spec.warm_cache {
            "Harness behavior: pre-read `<dest>/testfile.dat` once before launching fio to warm the page cache.".to_string()
        } else if spec.seed_existing_file {
            "Harness behavior: create the sandbox-local backing file before the timed run, then have the parent/backend drop page cache before fio starts.".to_string()
        } else {
            "Harness behavior: no extra cold/warm cache preparation is applied.".to_string()
        }),
        execution: format!(
            "Command:\nfio --output-format=json {}\n\nJobfile:\n```ini\n{}\n```",
            jobfile_path.display(),
            build_fio_job(spec, &file_path)
        ),
        source_path: source_path.to_string(),
        caveat: None,
    }
}

pub fn rust_execution(code: &str) -> String {
    format!("Rust code:\n```rust\n{}\n```", code.trim())
}

/// Create `count` files of `size` bytes each in `dir`, named `file-NNNN.dat`.
pub fn populate_files(dir: &Path, count: usize, size: usize) -> Result<()> {
    std::fs::create_dir_all(dir).context("creating base work dir")?;
    let buf = vec![0u8; size];
    for i in 0..count {
        std::fs::write(dir.join(format!("file-{i:06}.dat")), &buf)
            .with_context(|| format!("populating file-{i:06}.dat"))?;
    }
    Ok(())
}

pub fn allow_rw_rules(session_root: &Path) -> Vec<(String, Perm)> {
    vec![(session_root.to_string_lossy().into_owned(), Perm::AllowRw)]
}

pub fn emit_op_result(result: &OpResult) -> Result<()> {
    let json = serde_json::to_string(result).context("serialising OpResult")?;
    let stdout = io::stdout();
    let mut out = stdout.lock();
    writeln!(out, "{}", backend::RESULTS_MARKER)?;
    writeln!(out, "{json}")?;
    out.flush()?;
    Ok(())
}

pub fn summarize_latencies(
    mut latencies: Vec<Duration>,
    total: Duration,
    throughput_kbps: Option<u64>,
) -> OpResult {
    latencies.sort_unstable();
    let len = latencies.len();
    let percentile_us = |numerator: usize, denominator: usize| -> f64 {
        let idx = ((len * numerator).div_ceil(denominator)).saturating_sub(1);
        latencies[idx.min(len.saturating_sub(1))].as_secs_f64() * 1_000_000.0
    };

    let mean_us = latencies.iter().map(|d| d.as_secs_f64()).sum::<f64>() / len as f64 * 1_000_000.0;

    OpResult {
        iops: len as f64 / total.as_secs_f64(),
        throughput_kbps,
        lat_us_mean: mean_us,
        lat_us_p50: percentile_us(50, 100),
        lat_us_p99: percentile_us(99, 100),
        lat_us_p999: percentile_us(999, 1000),
        read_avg_lat_us: None,
        write_avg_lat_us: None,
        read_lat_us_p50: None,
        read_lat_us_p99: None,
        write_lat_us_p50: None,
        write_lat_us_p99: None,
    }
}

pub fn create_seed_file(dir: &Path) -> Result<PathBuf> {
    std::fs::create_dir_all(dir).context("creating fio work dir")?;
    let path = dir.join(FIO_FILE_NAME);
    if path.exists() {
        return Ok(path);
    }

    const CHUNK_SIZE: usize = 1024 * 1024;
    const CHUNKS: usize = 1024;

    let file = File::create(&path).with_context(|| format!("creating {}", path.display()))?;
    let mut writer = BufWriter::new(file);
    let mut chunk = vec![0u8; CHUNK_SIZE];
    for (i, byte) in chunk.iter_mut().enumerate() {
        *byte = ((i * 131 + 17) % 251 + 1) as u8;
    }
    for _ in 0..CHUNKS {
        writer
            .write_all(&chunk)
            .with_context(|| format!("writing patterned seed file {}", path.display()))?;
    }
    writer
        .flush()
        .with_context(|| format!("flushing patterned seed file {}", path.display()))?;
    Ok(path)
}

pub fn prepare_seeded_fio_workdir(dest: &Path) -> Result<()> {
    create_seed_file(dest).map(|_| ())
}

pub fn warm_file(path: &Path) -> Result<()> {
    let mut file = File::open(path).with_context(|| format!("opening {}", path.display()))?;
    io::copy(&mut file, &mut io::sink()).context("warming file in page cache")?;
    Ok(())
}

pub fn drop_page_cache() -> Result<()> {
    // Two rounds: the first sync+drop may not evict all metadata if ext4's
    // journal commit is still in flight (dirty buffers are skipped by
    // drop_caches). The brief pause lets the journal flush, then the second
    // drop evicts the now-clean buffers.
    for _ in 0..2 {
        let sync_status = Command::new("sync")
            .status()
            .context("running sync before dropping caches")?;
        if !sync_status.success() {
            bail!("sync failed before dropping caches");
        }
        let status = Command::new("sudo")
            .args(["sh", "-c", "echo 3 > /proc/sys/vm/drop_caches"])
            .status()
            .context("dropping page cache with sudo")?;
        if !status.success() {
            bail!("sudo drop_caches failed");
        }
    }
    Ok(())
}

pub fn run_fio(spec: FioSpec, dest: &Path, verbose: bool) -> Result<()> {
    std::fs::create_dir_all(dest).context("creating fio work dir")?;
    let testfile = dest.join(FIO_FILE_NAME);

    if spec.seed_existing_file && !testfile.exists() {
        bail!(
            "expected seeded fio backing file at {}, but it was not prepared",
            testfile.display()
        );
    }
    if spec.warm_cache {
        warm_file(&testfile)?;
    }

    let jobfile = dest.join("job.fio");
    std::fs::write(&jobfile, build_fio_job(spec, &testfile)).context("writing fio jobfile")?;

    let output = Command::new("fio")
        .arg("--output-format=json")
        .arg(&jobfile)
        .stderr(if verbose {
            Stdio::inherit()
        } else {
            Stdio::piped()
        })
        .output()
        .context("running fio")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("fio failed: {stderr}");
    }

    let fio_json: Value = serde_json::from_slice(&output.stdout).context("parsing fio JSON")?;
    let result = parse_fio_result(&fio_json)?;
    emit_op_result(&result)
}

fn build_fio_job(spec: FioSpec, testfile: &Path) -> String {
    let mut job = format!(
        "[{name}]\n\
         filename={file}\n\
         rw={rw}\n\
         bs=4k\n\
         filesize={filesize}\n\
         io_size={io_size}\n\
         direct=0\n\
         invalidate=0\n\
         ioengine=psync\n",
        name = spec.name,
        file = testfile.display(),
        rw = spec.rw,
        filesize = FIO_FILE_SIZE,
        io_size = spec.io_size.unwrap_or(FIO_IO_SIZE),
    );

    if let Some(mix) = spec.mix_read_percent {
        job.push_str(&format!("rwmixread={mix}\n"));
    }

    job
}

fn parse_fio_result(fio_json: &Value) -> Result<OpResult> {
    let job = &fio_json["jobs"][0];
    let read = &job["read"];
    let write = &job["write"];

    let read_iops = read["iops"].as_f64().unwrap_or(0.0);
    let write_iops = write["iops"].as_f64().unwrap_or(0.0);
    let total_iops = read_iops + write_iops;
    let throughput_kbps =
        (read["bw_bytes"].as_u64().unwrap_or(0) + write["bw_bytes"].as_u64().unwrap_or(0)) / 1024;

    let read_lats = fio_percentiles(read);
    let write_lats = fio_percentiles(write);
    let latencies = if read_iops > 0.0 && write_iops > 0.0 {
        weighted_percentiles(read_lats, read_iops, write_lats, write_iops)
    } else if read_iops > 0.0 {
        read_lats
    } else {
        write_lats
    };

    Ok(OpResult {
        iops: total_iops,
        throughput_kbps: Some(throughput_kbps),
        lat_us_mean: 1_000_000.0 / total_iops,
        lat_us_p50: latencies.0,
        lat_us_p99: latencies.1,
        lat_us_p999: latencies.2,
        read_avg_lat_us: (read_iops > 0.0).then(|| 1_000_000.0 / read_iops),
        write_avg_lat_us: (write_iops > 0.0).then(|| 1_000_000.0 / write_iops),
        read_lat_us_p50: (read_iops > 0.0).then_some(read_lats.0),
        read_lat_us_p99: (read_iops > 0.0).then_some(read_lats.1),
        write_lat_us_p50: (write_iops > 0.0).then_some(write_lats.0),
        write_lat_us_p99: (write_iops > 0.0).then_some(write_lats.1),
    })
}

fn fio_percentiles(section: &Value) -> (f64, f64, f64) {
    let clat = &section["clat_ns"]["percentile"];
    let ns_to_us = |key: &str| clat[key].as_f64().unwrap_or(0.0) / 1000.0;
    (
        ns_to_us("50.000000"),
        ns_to_us("99.000000"),
        ns_to_us("99.900000"),
    )
}

fn weighted_percentiles(
    left: (f64, f64, f64),
    left_weight: f64,
    right: (f64, f64, f64),
    right_weight: f64,
) -> (f64, f64, f64) {
    let total = left_weight + right_weight;
    if total == 0.0 {
        return (0.0, 0.0, 0.0);
    }

    let combine = |l: f64, r: f64| ((l * left_weight) + (r * right_weight)) / total;
    (
        combine(left.0, right.0),
        combine(left.1, right.1),
        combine(left.2, right.2),
    )
}

pub fn populate_op_files(dir: &Path, count: usize) -> Result<()> {
    populate_files(dir, count, OP_FILE_SIZE)
}

/// Populate a single directory with `count` files for readdir benchmarks.
pub fn populate_readdir_dir(dir: &Path, count: usize) -> Result<()> {
    populate_files(dir, count, OP_FILE_SIZE)
}

pub fn warm_metadata(dest: &Path, count: usize) -> Result<()> {
    for i in 0..count {
        let path = dest.join(format!("file-{i:06}.dat"));
        let meta = std::fs::metadata(&path).with_context(|| format!("stat {}", path.display()))?;
        std::hint::black_box(meta);
    }
    Ok(())
}

/// Warm a single directory's readdir cache.
pub fn warm_readdir_dir(dest: &Path, count: usize) -> Result<()> {
    let mut n = 0usize;
    for entry in std::fs::read_dir(dest).with_context(|| format!("reading {}", dest.display()))? {
        let entry = entry.with_context(|| format!("iterating {}", dest.display()))?;
        std::hint::black_box(entry.file_name());
        n += 1;
    }
    if n != count {
        bail!("expected {count} entries in {}, found {n}", dest.display());
    }
    Ok(())
}

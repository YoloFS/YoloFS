// agfs-bench — benchmark suite for the agfs filesystem.
//
// Usage:
//   agfs-bench [--workload <name> ...] [--backend <name>] [--verbose] [--timestamped-results]
//   agfs-bench rerender
//   agfs-bench exec-workload --name <name> --dest <path>

mod backend;
mod backends;
mod paper;
mod profiler;
mod report;
mod workload;
mod workloads;

use anyhow::{Context, Result, bail};
use backend::Backend;
use clap::{Parser, Subcommand, ValueEnum};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};
#[allow(unused_imports)]
use workload::{IterResult, Workload};

// ── CLI ───────────────────────────────────────────────────────────────────────

#[derive(Parser)]
#[command(name = "agfs-bench", about = "agfs benchmark suite")]
struct Cli {
    #[command(subcommand)]
    cmd: Option<Cmd>,

    /// Run only these workloads (repeatable)
    #[arg(long)]
    workload: Vec<String>,

    /// Run only session microbenchmarks
    #[arg(long, conflicts_with_all = ["workload", "macro", "op"])]
    micro: bool,

    /// Run only session macrobenchmarks
    #[arg(long, name = "macro", conflicts_with_all = ["workload", "micro", "op"])]
    r#macro: bool,

    /// Run only per-operation benchmarks (fio + metadata)
    #[arg(long, conflicts_with_all = ["workload", "micro", "macro"])]
    op: bool,

    /// With --op, narrow the selection to metadata or fio workloads
    #[arg(long, requires = "op")]
    op_group: Option<OpGroup>,

    /// Run only this backend
    #[arg(long)]
    backend: Option<String>,

    /// Number of timed iterations per (workload, backend); one warm-up run precedes these
    #[arg(long, default_value_t = 3)]
    runs: usize,

    /// Capture detailed logs for all runs, not just failures
    #[arg(long)]
    verbose: bool,

    /// Write results into a timestamped subdirectory instead of overwriting
    #[arg(long)]
    timestamped_results: bool,

    /// Skip (workload, backend) pairs that already have exactly --runs timed iterations in results.json
    #[arg(long)]
    skip_complete: bool,

    /// Skip (workload, backend) pairs whose recorded repo state matches the
    /// current checkout (cli/ and kmod/ unchanged). Useful for rerunning only
    /// workloads affected by code changes.
    #[arg(long)]
    skip_fresh: bool,
}

#[derive(Clone, Copy, ValueEnum)]
enum OpGroup {
    Meta,
    Fio,
}

#[derive(Subcommand)]
enum Cmd {
    /// Regenerate the HTML report from existing results JSON without re-running benchmarks
    Rerender {
        /// Only regenerate paper artifacts (no Plotly HTML workload/index pages)
        #[arg(long)]
        paper_only: bool,
    },
    /// List available workloads and backends
    List,
    /// Profile a workload with perf flamegraph (and optionally bpftrace)
    Profile {
        /// Workload to profile (required)
        #[arg(long)]
        workload: String,
        /// Backend to profile (default: agfs-no-perm + agfs-realistic)
        #[arg(long)]
        backend: Option<String>,
        /// Enable bpftrace op-latency histograms (default: perf flamegraph only)
        #[arg(long)]
        bpftrace: bool,
    },
    /// Visual diff of a PDF between two git commits
    DiffPdf {
        /// Path to the PDF file (relative to repo root)
        path: PathBuf,
        /// Old commit (default: HEAD~1)
        #[arg(long, default_value = "HEAD~1")]
        old: String,
        /// New commit (default: HEAD)
        #[arg(long, default_value = "HEAD")]
        new: String,
        /// Output PNG path (default: diff-<stem>.png in current dir)
        #[arg(long)]
        output: Option<PathBuf>,
    },
    /// Install preferred paper artifacts (tables + figures) into the paper repo
    InstallPaper {
        /// Path to the paper repository root (default: ../AgFS-paper)
        #[arg(long, default_value = "paper")]
        paper_dir: PathBuf,
    },
    /// Run a workload at a given path (used internally by all backends)
    ExecWorkload {
        /// Workload name
        #[arg(long)]
        name: String,
        /// Destination path for the workload
        #[arg(long)]
        dest: PathBuf,
        /// Verbose output
        #[arg(long)]
        verbose: bool,
        /// Block on stdin after printing READY, so the parent can drop
        /// page caches before the workload runs.
        #[arg(long)]
        wait_after_ready: bool,
    },
    /// Mount overlayfs then run a workload (used internally by the overlayfs backend)
    #[command(hide = true)]
    ExecOverlayfs {
        #[arg(long)]
        name: String,
        #[arg(long)]
        lower: PathBuf,
        #[arg(long)]
        upper: PathBuf,
        #[arg(long)]
        work: PathBuf,
        #[arg(long)]
        merged: PathBuf,
        #[arg(long)]
        verbose: bool,
        #[arg(long)]
        prepare_only: bool,
        /// Run prepare_workdir inside this overlay mount before the timed
        /// workload so that stage-local files stay in the upper layer.
        #[arg(long)]
        inline_prepare: bool,
        /// Block on stdin after printing READY, so the parent can drop
        /// page caches before the workload runs.
        #[arg(long)]
        wait_after_ready: bool,
        /// Unmount the overlay before READY and remount after GO, so
        /// overlay kernel state is fully flushed during drop_caches.
        #[arg(long)]
        remount_for_cold: bool,
    },
}

// ── Types ─────────────────────────────────────────────────────────────────────

#[derive(Serialize, Deserialize, Clone)]
struct RepoState {
    commit: String,
    cli_dirty: bool,
    kmod_dirty: bool,
}

#[derive(Serialize, Deserialize, Clone)]
struct Env {
    hostname: String,
    cpu: String,
    memory_gb: u64,
    storage: String,
    storage_device: String,
    storage_device_model: String,
    filesystem: String,
    filesystem_size_gb: u64,
    filesystem_free_gb: u64,
    mount_options: String,
    kernel: String,
    distro: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    cloudlab_cluster: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    cloudlab_hardware: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    repo_state: Option<RepoState>,
}

#[derive(Serialize, Deserialize, Clone)]
struct BackendResult {
    backend: String,
    iterations: Vec<IterResult>,
    mean_total_ms: f64,
    stddev_total_ms: f64,
    mean_init_ms: Option<f64>,
    mean_staging_ms: Option<f64>,
    mean_commit_ms: Option<f64>,
    /// Indices of iterations (0-based) whose total_ms is more than 2σ from the mean.
    outlier_iter_indices: Vec<usize>,
    /// Kernel messages observed during the run (on failure or --verbose).
    kernel_messages: Vec<String>,
    // ── Op workload fields (present when workload kind == Op) ──
    #[serde(skip_serializing_if = "Option::is_none")]
    mean_iops: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stddev_iops: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    mean_throughput_kbps: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    mean_lat_us_p50: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    mean_lat_us_p99: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    mean_read_avg_lat_us: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stddev_read_avg_lat_us: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    mean_write_avg_lat_us: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stddev_write_avg_lat_us: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    mean_read_lat_us_p50: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    mean_read_lat_us_p99: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    mean_write_lat_us_p50: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    mean_write_lat_us_p99: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    repo_state: Option<RepoState>,
    #[serde(skip_serializing_if = "Option::is_none")]
    checkpoint_series: Option<crate::workload::CheckpointLatencySeries>,
}

#[derive(Serialize, Deserialize, Clone)]
struct WorkloadResult {
    workload: String,
    backends: Vec<BackendResult>,
}

#[derive(Serialize, Deserialize, Clone)]
struct BenchResults {
    timestamp: u64,
    env: Env,
    workloads: Vec<WorkloadResult>,
}

// ── Environment collection ────────────────────────────────────────────────────

fn collect_env() -> Env {
    let cache_dir = dirs_next::cache_dir().unwrap_or_else(|| PathBuf::from("/root"));
    let (storage_device, filesystem, filesystem_size_gb, filesystem_free_gb, mount_options) =
        read_fs_info(&cache_dir);

    let (cloudlab_cluster, cloudlab_hardware) = read_cloudlab_info();

    Env {
        hostname: read_hostname(),
        cpu: read_cpu_model(),
        memory_gb: read_memory_gb(),
        storage: read_storage_type(),
        storage_device_model: read_device_model(&storage_device),
        storage_device,
        filesystem,
        filesystem_size_gb,
        filesystem_free_gb,
        mount_options,
        kernel: read_kernel_version(),
        distro: read_distro(),
        cloudlab_cluster,
        cloudlab_hardware,
        repo_state: read_repo_state().ok(),
    }
}

fn read_hostname() -> String {
    nix::unistd::gethostname()
        .ok()
        .and_then(|h: std::ffi::OsString| h.into_string().ok())
        .unwrap_or_else(|| "unknown".to_string())
}

fn read_cpu_model() -> String {
    fs::read_to_string("/proc/cpuinfo")
        .unwrap_or_default()
        .lines()
        .find(|l| l.starts_with("model name"))
        .and_then(|l| l.split(':').nth(1))
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| "unknown".to_string())
}

fn read_memory_gb() -> u64 {
    fs::read_to_string("/proc/meminfo")
        .unwrap_or_default()
        .lines()
        .find(|l| l.starts_with("MemTotal:"))
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|s| s.parse::<u64>().ok())
        .map(|kb| kb / (1024 * 1024))
        .unwrap_or(0)
}

fn read_storage_type() -> String {
    let mut has_ssd = false;
    let mut has_hdd = false;
    if let Ok(entries) = fs::read_dir("/sys/block") {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with("loop")
                || name.starts_with("ram")
                || name.starts_with("dm-")
                || name.starts_with("zram")
            {
                continue;
            }
            let rot = entry.path().join("queue/rotational");
            match fs::read_to_string(&rot).as_deref().map(str::trim) {
                Ok("0") => has_ssd = true,
                Ok("1") => has_hdd = true,
                _ => {}
            }
        }
    }
    match (has_ssd, has_hdd) {
        (true, false) => "SSD".to_string(),
        (false, true) => "HDD".to_string(),
        (true, true) => "mixed (SSD+HDD)".to_string(),
        _ => "unknown".to_string(),
    }
}

/// Given a device path like `/dev/sda1` or `/dev/nvme0n1p2`, return the model
/// string from sysfs, or a generic fallback.
fn read_device_model(device: &str) -> String {
    // Strip /dev/ prefix.
    let name = device.strip_prefix("/dev/").unwrap_or(device);
    // Strip partition suffix: nvme0n1p2 → nvme0n1, sda1 → sda, vda1 → vda.
    let base = if let Some(pos) = name
        .find('p')
        .filter(|&i| name[..i].chars().last().is_some_and(|c| c.is_ascii_digit()))
    {
        &name[..pos]
    } else {
        name.trim_end_matches(|c: char| c.is_ascii_digit())
    };
    let model_path = format!("/sys/block/{base}/device/model");
    fs::read_to_string(&model_path)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "virtual disk".to_string())
}

/// Return (device, fstype, total_size_gb, free_gb, mount_options) for the filesystem containing `path`.
fn read_fs_info(path: &Path) -> (String, String, u64, u64, String) {
    let mounts = fs::read_to_string("/proc/mounts").unwrap_or_default();
    let path_str = path.to_string_lossy();

    let best = mounts
        .lines()
        .filter_map(|line| {
            let mut parts = line.split_whitespace();
            let device = parts.next()?;
            let mountpoint = parts.next()?;
            let fstype = parts.next()?;
            let options = parts.next()?;
            if path_str.starts_with(mountpoint) {
                Some((
                    mountpoint.len(),
                    device.to_string(),
                    fstype.to_string(),
                    options.to_string(),
                ))
            } else {
                None
            }
        })
        .max_by_key(|(len, _, _, _)| *len);

    let (device, fstype, options) = best
        .map(|(_, dev, fs, opts)| (dev, fs, opts))
        .unwrap_or_else(|| {
            (
                "unknown".to_string(),
                "unknown".to_string(),
                "unknown".to_string(),
            )
        });

    let (size_gb, free_gb) = nix::sys::statvfs::statvfs(path)
        .ok()
        .map(|s| {
            let frsize = s.fragment_size();
            let size = s.blocks().saturating_mul(frsize) / (1024 * 1024 * 1024);
            let free = s.blocks_available().saturating_mul(frsize) / (1024 * 1024 * 1024);
            (size, free)
        })
        .unwrap_or((0, 0));

    (device, fstype, size_gb, free_gb, options)
}

fn read_kernel_version() -> String {
    nix::sys::utsname::uname()
        .ok()
        .map(|u| u.release().to_string_lossy().into_owned())
        .unwrap_or_else(|| "unknown".to_string())
}

fn read_distro() -> String {
    fs::read_to_string("/etc/os-release")
        .unwrap_or_default()
        .lines()
        .find(|l| l.starts_with("PRETTY_NAME="))
        .and_then(|l| l.split('=').nth(1))
        .map(|s| s.trim_matches('"').to_string())
        .unwrap_or_else(|| "unknown".to_string())
}

/// If running on CloudLab, return (cluster, hardware_type) from `geni-get`.
fn read_cloudlab_info() -> (Option<String>, Option<String>) {
    let manifest = Command::new("geni-get")
        .arg("manifest")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned());

    let manifest = match manifest {
        Some(m) => m,
        None => return (None, None),
    };

    let cluster = manifest
        .split("IDN+")
        .nth(1)
        .and_then(|s| s.split('+').next())
        .map(|s| s.to_string());

    let hardware = manifest
        .split("hardware_type name=\"")
        .nth(1)
        .and_then(|s| s.split('"').next())
        .map(|s| s.to_string());

    (cluster, hardware)
}

fn read_repo_state() -> Result<RepoState> {
    let root = repo_root();
    let commit = git_stdout(&root, &["rev-parse", "HEAD"]).context("reading git HEAD")?;
    Ok(RepoState {
        commit,
        cli_dirty: git_path_dirty(&root, "cli")?,
        kmod_dirty: git_path_dirty(&root, "kmod")?,
    })
}

fn git_stdout(root: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(root)
        .output()
        .with_context(|| format!("running git {}", args.join(" ")))?;
    if !output.status.success() {
        bail!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn git_path_dirty(root: &Path, path: &str) -> Result<bool> {
    let output = Command::new("git")
        .args(["status", "--porcelain", "--", path])
        .current_dir(root)
        .output()
        .with_context(|| format!("checking git dirtiness for {path}"))?;
    if !output.status.success() {
        bail!(
            "git status for {path} failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(!output.stdout.is_empty())
}

pub(crate) fn repo_paths_changed_between(from_commit: &str, to_commit: &str) -> Result<bool> {
    let root = repo_root();
    let status = Command::new("git")
        .args([
            "diff",
            "--quiet",
            from_commit,
            to_commit,
            "--",
            "cli",
            "kmod",
        ])
        .current_dir(&root)
        .status()
        .with_context(|| {
            format!("checking git diff between {from_commit} and {to_commit} for cli/ and kmod/")
        })?;

    match status.code() {
        Some(0) => Ok(false),
        Some(1) => Ok(true),
        _ => bail!("git diff failed comparing {from_commit} and {to_commit}"),
    }
}

// ── Dependency checks ─────────────────────────────────────────────────────────

/// Check that fio is installed. Called before running any workload whose name
/// starts with "fio-". Fails hard — fio is a required dependency, not optional.
fn require_fio() -> Result<()> {
    use std::process::Command;
    if !Command::new("fio")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
    {
        bail!(
            "fio is not installed. Per-operation I/O benchmarks require fio.\n\
             Install with: sudo apt-get install fio\n\
             Or run: bench/install_deps.sh"
        );
    }
    Ok(())
}

// ── Backend runner ───────────────────────────────────────────────────────────

fn run_backend(
    backend: &dyn Backend,
    workload: &dyn Workload,
    verbose: bool,
    runs: usize,
    repo_state: &Option<RepoState>,
) -> Result<BackendResult> {
    eprintln!("  backend: {}", backend.name());

    let mut iterations = Vec::with_capacity(runs);
    let mut checkpoint_series_runs: Vec<crate::workload::CheckpointLatencySeries> = Vec::new();
    let mut all_kernel_msgs: Vec<String> = Vec::new();
    for i in 0..runs {
        eprint!("    iter {}/{}… ", i + 1, runs);
        let (mut result, kernel_msgs) = match backend.run_one(workload, verbose) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("failed: {e:#}");
                eprintln!("    rerunning with verbose logging…");
                backend
                    .run_one(workload, true)
                    .with_context(|| format!("iter {} (verbose rerun) failed", i + 1))?
            }
        };
        {
            let mut parts = Vec::new();
            if let Some(i) = result.init_ms {
                parts.push(format!("init {i}"));
            }
            if let Some(s) = result.staging_ms {
                parts.push(format!("stage {s}"));
            }
            if let Some(c) = result.commit_ms {
                parts.push(format!("commit {c}"));
            }
            if parts.is_empty() {
                eprintln!("{} ms", result.total_ms);
            } else {
                eprintln!("{} ms  ({})", result.total_ms, parts.join(" + "));
            }
        }
        if !kernel_msgs.is_empty() || verbose {
            all_kernel_msgs.extend(kernel_msgs);
        }
        if let Some(series) = result.checkpoint_series.take() {
            checkpoint_series_runs.push(series);
        }
        iterations.push(result);
    }

    let stats = compute_stats(&iterations);

    if !stats.outlier_iter_indices.is_empty() {
        eprintln!(
            "    outliers (>2σ) at iterations: {:?}",
            stats
                .outlier_iter_indices
                .iter()
                .map(|i| i + 1)
                .collect::<Vec<_>>()
        );
    }

    Ok(BackendResult {
        backend: backend.name().to_string(),
        iterations,
        mean_total_ms: stats.mean_total,
        stddev_total_ms: stats.stddev_total,
        mean_init_ms: stats.mean_init,
        mean_staging_ms: stats.mean_staging,
        mean_commit_ms: stats.mean_commit,
        outlier_iter_indices: stats.outlier_iter_indices,
        kernel_messages: all_kernel_msgs,
        mean_iops: None,
        stddev_iops: None,
        mean_throughput_kbps: None,
        mean_lat_us_p50: None,
        mean_lat_us_p99: None,
        mean_read_avg_lat_us: None,
        stddev_read_avg_lat_us: None,
        mean_write_avg_lat_us: None,
        stddev_write_avg_lat_us: None,
        mean_read_lat_us_p50: None,
        mean_read_lat_us_p99: None,
        mean_write_lat_us_p50: None,
        mean_write_lat_us_p99: None,
        repo_state: repo_state.clone(),
        checkpoint_series: aggregate_checkpoint_series(&checkpoint_series_runs),
    })
}

fn aggregate_checkpoint_series(
    runs: &[crate::workload::CheckpointLatencySeries],
) -> Option<crate::workload::CheckpointLatencySeries> {
    let first = runs.first()?;
    if first.points.is_empty() {
        return None;
    }
    let len = first.points.len();
    if runs.iter().any(|s| s.points.len() != len) {
        return None;
    }
    for idx in 0..len {
        let checkpoint = first.points[idx].checkpoint;
        if runs.iter().any(|s| s.points[idx].checkpoint != checkpoint) {
            return None;
        }
    }

    let n = runs.len() as f64;
    let mut points = Vec::with_capacity(len);
    for idx in 0..len {
        let checkpoint = first.points[idx].checkpoint;
        let mean = |f: fn(&crate::workload::CheckpointLatencyPoint) -> f64| -> f64 {
            runs.iter().map(|s| f(&s.points[idx])).sum::<f64>() / n
        };
        let mean_ms = |f: fn(&crate::workload::CheckpointLatencyPoint) -> u64| -> u64 {
            (runs.iter().map(|s| f(&s.points[idx]) as f64).sum::<f64>() / n) as u64
        };
        let mean_count = |f: fn(&crate::workload::CheckpointLatencyPoint) -> usize| -> usize {
            (runs.iter().map(|s| f(&s.points[idx]) as f64).sum::<f64>() / n) as usize
        };
        points.push(crate::workload::CheckpointLatencyPoint {
            checkpoint,
            stat_avg_lat_us: mean(|p| p.stat_avg_lat_us),
            readdir_avg_lat_us: mean(|p| p.readdir_avg_lat_us),
            unlink_avg_lat_us: mean(|p| p.unlink_avg_lat_us),
            read_avg_lat_us: mean(|p| p.read_avg_lat_us),
            create_avg_lat_us: mean(|p| p.create_avg_lat_us),
            overwrite_avg_lat_us: mean(|p| p.overwrite_avg_lat_us),
            file_count: mean_count(|p| p.file_count),
            checkpoint_ms: mean_ms(|p| p.checkpoint_ms),
        });
    }

    Some(crate::workload::CheckpointLatencySeries { points })
}

fn run_backend_op(
    backend: &dyn Backend,
    workload: &dyn Workload,
    verbose: bool,
    runs: usize,
    repo_state: &Option<RepoState>,
) -> Result<BackendResult> {
    use crate::workload::OpResult;

    eprintln!("  backend: {}", backend.name());

    let mut iterations = Vec::with_capacity(runs);
    let mut op_results: Vec<OpResult> = Vec::with_capacity(runs);

    for i in 0..runs {
        eprint!("    iter {}/{}… ", i + 1, runs);
        let (result, _) = match backend.run_one(workload, verbose) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("failed: {e:#}");
                eprintln!("    rerunning with verbose logging…");
                backend
                    .run_one(workload, true)
                    .with_context(|| format!("iter {} (verbose rerun) failed", i + 1))?
            }
        };

        let op = result
            .op_result
            .clone()
            .with_context(|| "op workload did not produce OpResult")?;

        if let Some(tp) = op.throughput_kbps {
            eprintln!(
                "{:.0} IOPS  {:.1} MB/s  (p50 {:.1} µs, p99 {:.1} µs)",
                op.iops,
                tp as f64 / 1024.0,
                op.lat_us_p50,
                op.lat_us_p99
            );
        } else {
            eprintln!(
                "{:.0} IOPS  (p50 {:.1} µs, p99 {:.1} µs)",
                op.iops, op.lat_us_p50, op.lat_us_p99
            );
        }

        iterations.push(result);
        op_results.push(op);
    }

    let n = runs as f64;
    let mean_iops = op_results.iter().map(|r| r.iops).sum::<f64>() / n;
    let var = op_results
        .iter()
        .map(|r| (r.iops - mean_iops).powi(2))
        .sum::<f64>()
        / n;

    let mean_tp: Option<u64> = if op_results[0].throughput_kbps.is_some() {
        Some(
            (op_results
                .iter()
                .map(|r| r.throughput_kbps.unwrap_or(0) as f64)
                .sum::<f64>()
                / n) as u64,
        )
    } else {
        None
    };

    let mean_read_avg_lat_us = op_results[0].read_avg_lat_us.map(|_| {
        op_results
            .iter()
            .map(|r| r.read_avg_lat_us.unwrap_or(0.0))
            .sum::<f64>()
            / n
    });
    let stddev_read_avg_lat_us = mean_read_avg_lat_us.map(|mean| {
        (op_results
            .iter()
            .map(|r| (r.read_avg_lat_us.unwrap_or(0.0) - mean).powi(2))
            .sum::<f64>()
            / n)
            .sqrt()
    });
    let mean_write_avg_lat_us = op_results[0].write_avg_lat_us.map(|_| {
        op_results
            .iter()
            .map(|r| r.write_avg_lat_us.unwrap_or(0.0))
            .sum::<f64>()
            / n
    });
    let stddev_write_avg_lat_us = mean_write_avg_lat_us.map(|mean| {
        (op_results
            .iter()
            .map(|r| (r.write_avg_lat_us.unwrap_or(0.0) - mean).powi(2))
            .sum::<f64>()
            / n)
            .sqrt()
    });
    let mean_read_lat_us_p50 = op_results[0].read_lat_us_p50.map(|_| {
        op_results
            .iter()
            .map(|r| r.read_lat_us_p50.unwrap_or(0.0))
            .sum::<f64>()
            / n
    });
    let mean_read_lat_us_p99 = op_results[0].read_lat_us_p99.map(|_| {
        op_results
            .iter()
            .map(|r| r.read_lat_us_p99.unwrap_or(0.0))
            .sum::<f64>()
            / n
    });
    let mean_write_lat_us_p50 = op_results[0].write_lat_us_p50.map(|_| {
        op_results
            .iter()
            .map(|r| r.write_lat_us_p50.unwrap_or(0.0))
            .sum::<f64>()
            / n
    });
    let mean_write_lat_us_p99 = op_results[0].write_lat_us_p99.map(|_| {
        op_results
            .iter()
            .map(|r| r.write_lat_us_p99.unwrap_or(0.0))
            .sum::<f64>()
            / n
    });

    Ok(BackendResult {
        backend: backend.name().to_string(),
        iterations,
        mean_total_ms: 0.0,
        stddev_total_ms: 0.0,
        mean_init_ms: None,
        mean_staging_ms: None,
        mean_commit_ms: None,
        outlier_iter_indices: vec![],
        kernel_messages: vec![],
        mean_iops: Some(mean_iops),
        stddev_iops: Some(var.sqrt()),
        mean_throughput_kbps: mean_tp,
        mean_lat_us_p50: Some(op_results.iter().map(|r| r.lat_us_p50).sum::<f64>() / n),
        mean_lat_us_p99: Some(op_results.iter().map(|r| r.lat_us_p99).sum::<f64>() / n),
        mean_read_avg_lat_us,
        stddev_read_avg_lat_us,
        mean_write_avg_lat_us,
        stddev_write_avg_lat_us,
        mean_read_lat_us_p50,
        mean_read_lat_us_p99,
        mean_write_lat_us_p50,
        mean_write_lat_us_p99,
        repo_state: repo_state.clone(),
        checkpoint_series: None,
    })
}

// ── Statistics ────────────────────────────────────────────────────────────────

struct Stats {
    mean_total: f64,
    stddev_total: f64,
    mean_init: Option<f64>,
    mean_staging: Option<f64>,
    mean_commit: Option<f64>,
    outlier_iter_indices: Vec<usize>,
}

fn mean_of(iters: &[IterResult], f: impl Fn(&IterResult) -> Option<u64>) -> Option<f64> {
    if f(&iters[0]).is_some() {
        let n = iters.len() as f64;
        Some(iters.iter().map(|r| f(r).unwrap() as f64).sum::<f64>() / n)
    } else {
        None
    }
}

fn compute_stats(iters: &[IterResult]) -> Stats {
    let n = iters.len() as f64;
    let totals: Vec<f64> = iters.iter().map(|r| r.total_ms as f64).collect();
    let mean_total = totals.iter().sum::<f64>() / n;
    let variance = totals.iter().map(|v| (v - mean_total).powi(2)).sum::<f64>() / n;
    let stddev_total = variance.sqrt();

    let outlier_iter_indices = totals
        .iter()
        .enumerate()
        .filter(|(_, v)| (*v - mean_total).abs() > 2.0 * stddev_total)
        .map(|(i, _)| i)
        .collect();

    Stats {
        mean_total,
        stddev_total,
        mean_init: mean_of(iters, |r| r.init_ms),
        mean_staging: mean_of(iters, |r| r.staging_ms),
        mean_commit: mean_of(iters, |r| r.commit_ms),
        outlier_iter_indices,
    }
}

// ── Output ────────────────────────────────────────────────────────────────────

/// Root of the repository, determined at compile time.
fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("bench crate should be inside repo")
        .to_path_buf()
}

fn results_dir(env: &Env, timestamped: bool) -> PathBuf {
    let dir_name = match (&env.cloudlab_hardware, &env.cloudlab_cluster) {
        (Some(hw), Some(cluster)) => format!("{hw}@{cluster}"),
        _ => env.hostname.clone(),
    };
    let base = repo_root().join("results-bench").join(dir_name);
    if timestamped {
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        base.join(ts.to_string())
    } else {
        base
    }
}

fn read_existing_results(json_path: &Path) -> Result<Option<BenchResults>> {
    if !json_path.exists() {
        return Ok(None);
    }

    let raw = fs::read_to_string(json_path).context("reading existing results.json")?;
    match serde_json::from_str(&raw) {
        Ok(results) => Ok(Some(results)),
        Err(_) => {
            eprintln!("Existing results.json uses old format; ignoring for resume/merge checks.");
            Ok(None)
        }
    }
}

fn has_exact_runs(
    results: &BenchResults,
    workload_name: &str,
    backend_name: &str,
    runs: usize,
) -> bool {
    results
        .workloads
        .iter()
        .find(|w| w.workload == workload_name)
        .and_then(|w| w.backends.iter().find(|b| b.backend == backend_name))
        .is_some_and(|b| b.iterations.len() == runs)
}

fn merge_results(existing: BenchResults, incoming: &BenchResults) -> BenchResults {
    let mut workload_map: std::collections::BTreeMap<String, WorkloadResult> = existing
        .workloads
        .into_iter()
        .map(|w| (w.workload.clone(), w))
        .collect();

    for new_wl in &incoming.workloads {
        match workload_map.get_mut(&new_wl.workload) {
            Some(existing_wl) => {
                let new_backend_names: std::collections::HashSet<&str> =
                    new_wl.backends.iter().map(|b| b.backend.as_str()).collect();
                existing_wl
                    .backends
                    .retain(|b| !new_backend_names.contains(b.backend.as_str()));
                existing_wl.backends.extend(new_wl.backends.iter().cloned());
            }
            None => {
                workload_map.insert(new_wl.workload.clone(), new_wl.clone());
            }
        }
    }

    BenchResults {
        timestamp: incoming.timestamp,
        env: incoming.env.clone(),
        workloads: workload_map.into_values().collect(),
    }
}

/// Check if a (workload, backend) result is fresh — its recorded repo state
/// matches the current one (no cli/ or kmod/ changes).
fn is_result_fresh(
    results: &BenchResults,
    workload_name: &str,
    backend_name: &str,
    current: &Option<RepoState>,
) -> bool {
    let Some(current) = current else {
        return false;
    };
    let recorded = results
        .workloads
        .iter()
        .find(|w| w.workload == workload_name)
        .and_then(|w| w.backends.iter().find(|b| b.backend == backend_name))
        .and_then(|b| b.repo_state.as_ref());
    let Some(recorded) = recorded else {
        return false;
    };
    if recorded.commit == current.commit
        && recorded.cli_dirty == current.cli_dirty
        && recorded.kmod_dirty == current.kmod_dirty
    {
        return true;
    }
    // Different commit but maybe cli/ and kmod/ are unchanged.
    if recorded.commit != current.commit {
        match repo_paths_changed_between(&recorded.commit, &current.commit) {
            Ok(false) => {
                // Same cli/kmod content despite different commit.
                return recorded.cli_dirty == current.cli_dirty
                    && recorded.kmod_dirty == current.kmod_dirty;
            }
            _ => return false,
        }
    }
    false
}

/// Remove a stale backend entry from results.json (e.g. when the combination
/// is now skipped as unsupported but old data exists).
fn remove_stale_backend(out_dir: &Path, workload_name: &str, backend_name: &str) -> Result<()> {
    let json_path = out_dir.join("results.json");
    if let Some(mut results) = read_existing_results(&json_path)? {
        let mut changed = false;
        for wl in &mut results.workloads {
            if wl.workload == workload_name {
                let before = wl.backends.len();
                wl.backends.retain(|b| b.backend != backend_name);
                if wl.backends.len() < before {
                    changed = true;
                }
            }
        }
        if changed {
            let json = serde_json::to_string_pretty(&results).context("serialising results")?;
            fs::write(&json_path, json).context("writing results.json")?;
        }
    }
    Ok(())
}

/// Write results to disk, merging with any existing data. Returns the merged results.
fn write_results(results: &BenchResults, out_dir: &Path) -> Result<BenchResults> {
    fs::create_dir_all(out_dir).context("creating results dir")?;
    let json_path = out_dir.join("results.json");

    // Merge with existing results: replace workload entries that were re-run,
    // preserve workload entries that were not part of this run.
    // If the existing file uses the old format (scenarios), skip merging.
    let merged = if let Some(existing) = read_existing_results(&json_path)? {
        merge_results(existing, results)
    } else {
        results.clone()
    };

    let json = serde_json::to_string_pretty(&merged).context("serialising results")?;
    fs::write(&json_path, json).context("writing results.json")?;
    eprintln!("Results written to {}", json_path.display());
    Ok(merged)
}

// ── Profile ───────────────────────────────────────────────────────────────────

fn run_profile(env: &Env, workload_name: &str, backend_name: &str, bpftrace: bool) -> Result<()> {
    let workload = workloads::by_name(workload_name)
        .with_context(|| format!("unknown workload: {workload_name}"))?;
    let backend = backends::by_name(backend_name)
        .with_context(|| format!("unknown backend: {backend_name}"))?;
    if !backend.available() {
        bail!(
            "backend {backend_name} is not available: {}",
            backend.unavailable_reason().unwrap_or("unknown")
        );
    }

    workload.ensure_fixture()?;

    let out_dir = results_dir(env, false)
        .join("profiling")
        .join(workload_name)
        .join(backend_name);

    // Set up a single session, run the workload repeatedly under perf.
    // Using run_one() would create a fresh session (mount/populate/commit)
    // per iteration, burying the actual workload under setup noise.
    eprintln!("Profiling {workload_name} / {backend_name} (setup)…");
    // Warm-up: one full run_one to populate caches.
    backend.run_one(workload.as_ref(), false)?;
    // Second run_one for the profiled session — we keep it alive and
    // re-run the workload in a subprocess loop under perf.
    // For now, use run_one in a loop but accept the setup overhead.
    // TODO: mount once, loop run() only.
    eprintln!("Profiling {workload_name} / {backend_name} (recording)…");
    let p = profiler::Profiler::start(&out_dir, bpftrace)?;
    let t0 = std::time::Instant::now();
    let min_profile_ms = 10000;
    let mut iters = 0u32;
    loop {
        backend.run_one(workload.as_ref(), false)?;
        iters += 1;
        if t0.elapsed().as_millis() as u64 >= min_profile_ms {
            break;
        }
    }
    let wall_ms = t0.elapsed().as_millis() as u64;
    p.stop(wall_ms, iters)?;

    Ok(())
}

// ── Main ──────────────────────────────────────────────────────────────────────

fn main() -> Result<()> {
    let cli = Cli::parse();

    ensure_release_build()?;

    // exec-workload: internal subcommand used by all backends to run the
    // workload in a subprocess. Prints a READY marker to stdout so the parent
    // can split init vs staging time.
    if let Some(Cmd::ExecWorkload {
        name,
        dest,
        verbose,
        wait_after_ready,
    }) = &cli.cmd
    {
        let workload =
            workloads::by_name(name).with_context(|| format!("unknown workload: {name}"))?;
        // dest is already created by the parent backend before spawning
        // this subprocess. Skipping create_dir_all here avoids warming
        // the dcache/icache path for cold-cache workloads.
        println!("{}", backend::READY_MARKER);
        if *wait_after_ready {
            // Block until the parent signals (after dropping page caches).
            let mut buf = String::new();
            std::io::stdin().read_line(&mut buf)?;
        }
        workload.run(dest, *verbose)?;
        return Ok(());
    }

    // exec-overlayfs: mount overlayfs inside a user namespace, then run the
    // workload. Called by the overlayfs backend via unshare(1).
    if let Some(Cmd::ExecOverlayfs {
        name,
        lower,
        upper,
        work,
        merged,
        verbose,
        prepare_only,
        inline_prepare,
        wait_after_ready,
        remount_for_cold,
    }) = &cli.cmd
    {
        use nix::mount::{MsFlags, mount};
        let opts = format!(
            "userxattr,lowerdir={},upperdir={},workdir={}",
            lower.display(),
            upper.display(),
            work.display()
        );
        mount(
            Some("overlay"),
            merged.as_path(),
            Some("overlay"),
            MsFlags::empty(),
            Some(opts.as_str()),
        )
        .with_context(|| format!("mounting overlayfs on {}", merged.display()))?;

        let workload =
            workloads::by_name(name).with_context(|| format!("unknown workload: {name}"))?;
        let dest = merged.join(workload.work_dir());
        std::fs::create_dir_all(&dest)?;
        if *prepare_only {
            workload.prepare_workdir(&dest)?;
            return Ok(());
        }
        // For stage workloads: prepare inside this mount so files stay
        // in the upper layer rather than being committed to lower.
        if *inline_prepare && workload.needs_prepare_workdir() {
            workload.prepare_workdir(&dest)?;
        }
        if *remount_for_cold {
            // Unmount the overlay so its kernel state is flushed during
            // the parent's drop_caches. Remount after receiving GO.
            nix::mount::umount(merged.as_path())
                .with_context(|| format!("unmounting overlayfs on {}", merged.display()))?;
        }
        println!("{}", backend::READY_MARKER);
        if *wait_after_ready {
            let mut buf = String::new();
            std::io::stdin().read_line(&mut buf)?;
        }
        if *remount_for_cold {
            mount(
                Some("overlay"),
                merged.as_path(),
                Some("overlay"),
                MsFlags::empty(),
                Some(opts.as_str()),
            )
            .with_context(|| format!("remounting overlayfs on {}", merged.display()))?;
        }
        workload.run(&dest, *verbose)?;
        return Ok(());
    }

    let env = collect_env();

    if let Some(Cmd::Profile {
        workload: wname,
        backend: bname,
        bpftrace,
    }) = cli.cmd
    {
        let backends: Vec<&str> = match bname.as_deref() {
            Some(b) => vec![b],
            None => vec!["agfs-no-perm", "agfs-realistic"],
        };
        for bname in backends {
            run_profile(&env, &wname, bname, bpftrace)?;
        }
        // Generate cross-backend comparison if multiple backends were profiled.
        let prof_workload_dir = results_dir(&env, false).join("profiling").join(&wname);
        if prof_workload_dir.exists() {
            profiler::generate_comparison(&prof_workload_dir)?;
        }
        return Ok(());
    }

    if let Some(Cmd::List) = cli.cmd {
        println!("Workloads (micro):");
        for w in workloads::by_kind(workload::WorkloadKind::Micro) {
            println!("  {}", w.name());
        }
        println!("\nWorkloads (macro):");
        for w in workloads::by_kind(workload::WorkloadKind::Macro) {
            println!("  {}", w.name());
        }
        println!("\nWorkloads (op):");
        for w in workloads::by_kind(workload::WorkloadKind::Op) {
            println!("  {}", w.name());
        }
        let source_groups = workloads::source_groups();
        if !source_groups.is_empty() {
            println!("\nWorkload groups (expand to source variants):");
            for group in source_groups {
                println!("  {}", group);
            }
        }
        println!("\nBackends:");
        for b in backends::all() {
            if b.hidden() {
                // Don't probe availability for hidden backends.
                println!("  {} (hidden)", b.name());
            } else if !b.available() {
                let reason = b.unavailable_reason().unwrap_or("missing required tools");
                println!("  {} (unavailable: {})", b.name(), reason);
            } else {
                println!("  {}", b.name());
            }
        }
        return Ok(());
    }

    if let Some(Cmd::DiffPdf { path, old, new, output }) = cli.cmd {
        diff_pdf(&path, &old, &new, output.as_deref())?;
        return Ok(());
    }

    if let Some(Cmd::InstallPaper { paper_dir }) = cli.cmd {
        let out_dir = results_dir(&env, false);
        let results_path = out_dir.join("results.json");
        let json = fs::read_to_string(&results_path)
            .with_context(|| format!("reading {}", results_path.display()))?;
        let results: BenchResults = serde_json::from_str(&json).context("parsing results.json")?;
        paper::install(&results, &out_dir, &paper_dir)?;
        return Ok(());
    }

    if let Some(Cmd::Rerender { paper_only }) = cli.cmd {
        let out_dir = results_dir(&env, false);
        let results_path = out_dir.join("results.json");
        let json = fs::read_to_string(&results_path)
            .with_context(|| format!("reading {}", results_path.display()))?;
        let results: BenchResults = serde_json::from_str(&json).context("parsing results.json")?;
        if paper_only {
            report::render_paper_only(&results, &out_dir)?;
        } else {
            report::render(&results, &out_dir)?;
        }
        return Ok(());
    }

    let selected_workloads: Vec<Box<dyn Workload>> = if !cli.workload.is_empty() {
        let mut selected = Vec::new();
        for name in &cli.workload {
            let expanded = workloads::expand_selector(name);
            if expanded.is_empty() {
                bail!("unknown workload: {name}");
            }
            selected.extend(expanded);
        }
        selected.sort_by_key(|w| {
            workloads::all()
                .iter()
                .position(|ww| ww.name() == w.name())
                .unwrap_or(usize::MAX)
        });
        selected.dedup_by(|a, b| a.name() == b.name());
        selected
    } else if cli.micro {
        workloads::by_kind(workload::WorkloadKind::Micro)
    } else if cli.r#macro {
        workloads::by_kind(workload::WorkloadKind::Macro)
    } else if cli.op {
        let mut selected = workloads::by_kind(workload::WorkloadKind::Op);
        selected.retain(|w| !w.hidden());
        if let Some(group) = cli.op_group {
            selected.retain(|w| match group {
                OpGroup::Meta => w.name().starts_with("meta-"),
                OpGroup::Fio => w.name().starts_with("fio-"),
            });
        }
        selected
    } else {
        workloads::all()
            .into_iter()
            .filter(|w| !w.hidden())
            .collect()
    };

    // Fail hard if any selected workload needs fio and it's not installed.
    if selected_workloads
        .iter()
        .any(|w| w.name().starts_with("fio-"))
    {
        require_fio()?;
    }

    let selected_backends: Vec<Box<dyn Backend>> = if let Some(name) = &cli.backend {
        // Explicit --backend: run it even if hidden, just check availability.
        let b = backends::by_name(name).with_context(|| format!("unknown backend: {name}"))?;
        if !b.available() {
            bail!("backend {name} is not available (missing required tools)");
        }
        vec![b]
    } else {
        let all = backends::all();
        for b in &all {
            if b.hidden() {
                continue;
            }
            if !b.available() {
                let reason = b.unavailable_reason().unwrap_or("missing required tools");
                eprintln!("Skipping backend '{}': {}", b.name(), reason);
            }
        }
        all.into_iter()
            .filter(|b| !b.hidden() && b.available())
            .collect()
    };

    let out_dir = results_dir(&env, cli.timestamped_results);
    let json_path = out_dir.join("results.json");

    for workload in &selected_workloads {
        eprintln!("Running workload: {}", workload.name());
        workload.ensure_fixture()?;

        let is_op = workload.kind() == workload::WorkloadKind::Op;
        let mut did_warm_up = false;
        for b in &selected_backends {
            if cli.skip_complete
                && let Some(existing) = read_existing_results(&json_path)?
                && has_exact_runs(&existing, workload.name(), b.name(), cli.runs)
            {
                eprintln!(
                    "  backend: {} (skipping, already has {} timed iterations)",
                    b.name(),
                    cli.runs
                );
                continue;
            }

            if cli.skip_fresh
                && let Some(existing) = read_existing_results(&json_path)?
                && is_result_fresh(&existing, workload.name(), b.name(), &env.repo_state)
            {
                eprintln!("  backend: {} (skipping, result is fresh)", b.name());
                continue;
            }

            if let Some(reason) = b.unsupported_reason(workload.as_ref()) {
                eprintln!("  backend: {} (skipping: {})", b.name(), reason);
                // Remove stale data from results.json so the report shows N/A.
                remove_stale_backend(&out_dir, workload.name(), b.name())?;
                continue;
            }

            if !did_warm_up {
                eprintln!("  warm-up…");
                let native = backends::native::Native;
                native
                    .run_one(workload.as_ref(), cli.verbose)
                    .context("warm-up iteration failed")?;
                did_warm_up = true;
            }

            let result = if is_op {
                run_backend_op(
                    b.as_ref(),
                    workload.as_ref(),
                    cli.verbose,
                    cli.runs,
                    &env.repo_state,
                )?
            } else {
                run_backend(
                    b.as_ref(),
                    workload.as_ref(),
                    cli.verbose,
                    cli.runs,
                    &env.repo_state,
                )?
            };
            let timestamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            let partial = BenchResults {
                timestamp,
                env: env.clone(),
                workloads: vec![WorkloadResult {
                    workload: workload.name().to_string(),
                    backends: vec![result],
                }],
            };
            let merged = write_results(&partial, &out_dir)?;
            report::render_one(&merged, workload.name(), &out_dir)?;
        }
    }

    Ok(())
}

fn ensure_release_build() -> Result<()> {
    if cfg!(debug_assertions) {
        bail!("agfs-bench must be built with --release; debug builds are refused");
    }
    Ok(())
}

// ── diff-pdf ────────────────────────────────────────────────────────────────

fn diff_pdf(path: &Path, old_ref: &str, new_ref: &str, output: Option<&Path>) -> Result<()> {
    let tmp = tempfile::tempdir().context("creating temp dir")?;
    let stem = path.file_stem().unwrap().to_string_lossy();

    // Detect if the path is inside a submodule and resolve git commands there.
    let (git_dir, git_path) = resolve_git_context(path)?;

    // Extract PDF at each commit.
    let old_pdf = tmp.path().join(format!("{stem}-old.pdf"));
    let new_pdf = tmp.path().join(format!("{stem}-new.pdf"));
    git_show_to_file(old_ref, &git_path, &old_pdf, &git_dir)?;
    git_show_to_file(new_ref, &git_path, &new_pdf, &git_dir)?;

    // Convert to PNG with pdftoppm.
    let old_png = tmp.path().join(format!("{stem}-old"));
    let new_png = tmp.path().join(format!("{stem}-new"));
    pdftoppm(&old_pdf, &old_png)?;
    pdftoppm(&new_pdf, &new_png)?;

    // pdftoppm appends -1.png for single-page PDFs.
    let old_img_path = find_pdftoppm_output(&old_png)?;
    let new_img_path = find_pdftoppm_output(&new_png)?;

    // Generate visual diff in pure Rust.
    let out_path = if let Some(p) = output {
        p.to_path_buf()
    } else {
        let diff_dir = path.parent().unwrap_or(Path::new(".")).join("diff");
        std::fs::create_dir_all(&diff_dir)?;
        let short = |r: &str| {
            Command::new("git")
                .arg("-C").arg(&git_dir)
                .args(["rev-parse", "--short", r])
                .output()
                .ok()
                .filter(|o| o.status.success())
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
                .unwrap_or_else(|| r.replace('/', "_").replace('~', "_"))
        };
        diff_dir.join(format!("{stem}_{}_{}.png", short(old_ref), short(new_ref)))
    };

    // Load images and compute visual diff.
    let old_img = image::open(&old_img_path)
        .with_context(|| format!("opening {}", old_img_path.display()))?
        .to_rgb8();
    let new_img = image::open(&new_img_path)
        .with_context(|| format!("opening {}", new_img_path.display()))?
        .to_rgb8();

    // Pad to same size (white fill).
    let w = old_img.width().max(new_img.width());
    let h = old_img.height().max(new_img.height());
    let pad = |img: &image::RgbImage| -> image::RgbImage {
        let mut out = image::RgbImage::from_pixel(w, h, image::Rgb([255, 255, 255]));
        for (x, y, px) in img.enumerate_pixels() {
            out.put_pixel(x, y, *px);
        }
        out
    };
    let old_padded = pad(&old_img);
    let new_padded = pad(&new_img);

    // Build diff image.
    let mut out_img = image::RgbImage::new(w, h);
    let mut n_changed: u64 = 0;
    const THRESHOLD: u8 = 8;

    for y in 0..h {
        for x in 0..w {
            let op = old_padded.get_pixel(x, y).0;
            let np = new_padded.get_pixel(x, y).0;

            let max_diff = (0..3)
                .map(|c| (op[c] as i16 - np[c] as i16).unsigned_abs() as u8)
                .max()
                .unwrap();

            if max_diff > THRESHOLD {
                n_changed += 1;
                // Deleted (was dark, now white): blue tint.
                let old_bright = op.iter().copied().max().unwrap();
                let new_bright = np.iter().copied().max().unwrap();
                if old_bright < 250 && new_bright > 250 {
                    out_img.put_pixel(x, y, image::Rgb([80, 80, 220]));
                } else {
                    // Changed: red-tinted version of new pixel.
                    let r = ((np[0] as f32) * 0.5 + 128.0).min(255.0) as u8;
                    let g = ((np[1] as f32) * 0.3) as u8;
                    let b = ((np[2] as f32) * 0.3) as u8;
                    out_img.put_pixel(x, y, image::Rgb([r, g, b]));
                }
            } else {
                // Unchanged: dim.
                let r = ((np[0] as f32) * 0.3 + 180.0).min(255.0) as u8;
                let g = ((np[1] as f32) * 0.3 + 180.0).min(255.0) as u8;
                let b = ((np[2] as f32) * 0.3 + 180.0).min(255.0) as u8;
                out_img.put_pixel(x, y, image::Rgb([r, g, b]));
            }
        }
    }

    out_img.save(&out_path)
        .with_context(|| format!("writing {}", out_path.display()))?;

    let pct = n_changed as f64 / (w as f64 * h as f64) * 100.0;
    eprintln!("{n_changed} pixels changed ({pct:.1}% of {w}x{h})");
    eprintln!(
        "Diff: {old_ref}..{new_ref} {}\n  → {}",
        path.display(),
        out_path.display()
    );

    Ok(())
}

fn git_show_to_file(rev: &str, path: &Path, dest: &Path, git_dir: &Path) -> Result<()> {
    let spec = format!("{rev}:{}", path.display());
    let out = Command::new("git")
        .arg("-C").arg(git_dir)
        .args(["show", &spec])
        .output()
        .with_context(|| format!("git -C {} show {spec}", git_dir.display()))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        bail!("git show {spec} failed: {stderr}");
    }
    std::fs::write(dest, &out.stdout)
        .with_context(|| format!("writing {}", dest.display()))?;
    Ok(())
}

/// Given a file path, figure out the git repo root and the path relative to it.
/// Handles submodules: if the path is inside a submodule, returns the submodule
/// root and the path relative to it.
fn resolve_git_context(path: &Path) -> Result<(PathBuf, PathBuf)> {
    // Walk up from the file's directory to find the nearest .git (file or dir).
    let abs = std::fs::canonicalize(path)
        .or_else(|_| {
            // File might not exist on disk (only in git). Use the parent dir.
            let parent = path.parent().unwrap_or(Path::new("."));
            let canon_parent = std::fs::canonicalize(parent)?;
            Ok::<_, std::io::Error>(canon_parent.join(path.file_name().unwrap()))
        })
        .with_context(|| format!("resolving {}", path.display()))?;

    let dir = abs.parent().unwrap_or(Path::new("."));
    let out = Command::new("git")
        .arg("-C").arg(dir)
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .context("git rev-parse --show-toplevel")?;
    if !out.status.success() {
        bail!("{} is not in a git repository", path.display());
    }
    let repo_root = PathBuf::from(String::from_utf8_lossy(&out.stdout).trim());
    let rel = abs.strip_prefix(&repo_root)
        .with_context(|| format!("{} not under {}", abs.display(), repo_root.display()))?;
    Ok((repo_root, rel.to_path_buf()))
}

fn pdftoppm(pdf: &Path, out_prefix: &Path) -> Result<()> {
    let out = Command::new("pdftoppm")
        .args(["-png", "-r", "300"])
        .arg(pdf)
        .arg(out_prefix)
        .output()
        .context("running pdftoppm")?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        bail!("pdftoppm failed: {stderr}");
    }
    Ok(())
}

fn find_pdftoppm_output(prefix: &Path) -> Result<PathBuf> {
    // pdftoppm outputs <prefix>-1.png, <prefix>-01.png, or <prefix>.png
    let dir = prefix.parent().unwrap();
    let stem = prefix.file_name().unwrap().to_string_lossy();
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with(stem.as_ref()) && name.ends_with(".png") {
            return Ok(entry.path());
        }
    }
    bail!("pdftoppm output not found for {}", prefix.display())
}

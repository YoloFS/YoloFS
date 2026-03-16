// agfs-bench — benchmark suite for the agfs filesystem.
//
// Usage:
//   agfs-bench [--workload <name>] [--backend <name>] [--verbose] [--timestamped-results]
//   agfs-bench rerender
//   agfs-bench exec-workload --name <name> --dest <path>

mod backend;
mod backends;
mod profiler;
mod report;
mod workload;
mod workloads;

use anyhow::{Context, Result, bail};
use backend::Backend;
use clap::{Parser, Subcommand};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
#[allow(unused_imports)]
use workload::{IterResult, Workload};

// ── CLI ───────────────────────────────────────────────────────────────────────

#[derive(Parser)]
#[command(name = "agfs-bench", about = "agfs benchmark suite")]
struct Cli {
    #[command(subcommand)]
    cmd: Option<Cmd>,

    /// Run only this workload
    #[arg(long)]
    workload: Option<String>,

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
}

#[derive(Subcommand)]
enum Cmd {
    /// Regenerate the HTML report from existing results JSON without re-running benchmarks
    Rerender,
    /// List available workloads and backends
    List,
    /// Profile a workload with bpftrace and perf (agfs backends only)
    Profile {
        /// Workload to profile (required)
        #[arg(long)]
        workload: String,
        /// Scenario to profile (default: all agfs scenarios)
        #[arg(long)]
        scenario: Option<String>,
        /// Disable bpftrace op-latency histograms; only run perf for a clean flamegraph
        #[arg(long)]
        no_bpftrace: bool,
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
    },
}

// ── Types ─────────────────────────────────────────────────────────────────────

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

// ── Backend runner ───────────────────────────────────────────────────────────

fn run_backend(
    backend: &dyn Backend,
    workload: &dyn Workload,
    verbose: bool,
    runs: usize,
) -> Result<BackendResult> {
    eprintln!("  backend: {}", backend.name());

    let mut iterations = Vec::with_capacity(runs);
    let mut all_kernel_msgs: Vec<String> = Vec::new();
    for i in 0..runs {
        eprint!("    iter {}/{}… ", i + 1, runs);
        let (result, kernel_msgs) = match backend.run_one(workload, verbose) {
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
}

fn results_dir(hostname: &str, timestamped: bool) -> PathBuf {
    let base = repo_root().join("results-bench").join(hostname);
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

fn write_fresh(results: &BenchResults, json_path: &Path) -> Result<BenchResults> {
    let json = serde_json::to_string_pretty(results).context("serialising results")?;
    fs::write(json_path, json).context("writing results.json")?;
    eprintln!("Results written to {}", json_path.display());
    Ok(results.clone())
}

/// Write results to disk, merging with any existing data. Returns the merged results.
fn write_results(results: &BenchResults, out_dir: &Path) -> Result<BenchResults> {
    fs::create_dir_all(out_dir).context("creating results dir")?;
    let json_path = out_dir.join("results.json");

    // Merge with existing results: replace workload entries that were re-run,
    // preserve workload entries that were not part of this run.
    // If the existing file uses the old format (scenarios), skip merging.
    let merged = if json_path.exists() {
        let raw = fs::read_to_string(&json_path).context("reading existing results.json")?;
        let existing: BenchResults = match serde_json::from_str(&raw) {
            Ok(r) => r,
            Err(_) => {
                eprintln!("Existing results.json uses old format; overwriting.");
                return write_fresh(results, &json_path);
            }
        };

        // Build a map of existing workloads for merging.
        let mut workload_map: std::collections::BTreeMap<String, WorkloadResult> = existing
            .workloads
            .into_iter()
            .map(|w| (w.workload.clone(), w))
            .collect();

        for new_wl in &results.workloads {
            match workload_map.get_mut(&new_wl.workload) {
                Some(existing_wl) => {
                    // Merge backends: replace re-run backends, keep the rest.
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

        let workloads: Vec<WorkloadResult> = workload_map.into_values().collect();

        BenchResults {
            timestamp: results.timestamp,
            env: results.env.clone(),
            workloads,
        }
    } else {
        results.clone()
    };

    let json = serde_json::to_string_pretty(&merged).context("serialising results")?;
    fs::write(&json_path, json).context("writing results.json")?;
    eprintln!("Results written to {}", json_path.display());
    Ok(merged)
}

// ── Profile ───────────────────────────────────────────────────────────────────

fn run_profile(env: &Env, workload_name: &str, scenario_name: &str, bpftrace: bool) -> Result<()> {
    let workload = workloads::by_name(workload_name)
        .with_context(|| format!("unknown workload: {workload_name}"))?;

    let allow_all = match scenario_name {
        "agfs-allow-all" => true,
        "agfs-realistic" => false,
        other => bail!("unknown agfs scenario for profiling: {other}"),
    };

    workload.ensure_fixture()?;

    let out_dir = results_dir(&env.hostname, false)
        .join("profiling")
        .join(workload_name)
        .join(scenario_name);

    let (session, dest) = backends::agfs::setup_profile_session(workload.as_ref(), allow_all)?;
    std::fs::create_dir_all(&dest).context("creating workload dest dir")?;

    eprintln!("Profiling {workload_name} / {scenario_name}…");
    let p = profiler::Profiler::start(&out_dir, bpftrace)?;
    let t0 = std::time::Instant::now();
    workload.run(&dest, false)?;
    session.commit(false)?;
    let wall_ms = t0.elapsed().as_millis() as u64;
    p.stop(wall_ms)?;

    Ok(())
}

// ── Main ──────────────────────────────────────────────────────────────────────

fn main() -> Result<()> {
    let cli = Cli::parse();

    // exec-workload: internal subcommand used by all backends to run the
    // workload in a subprocess. Prints a READY marker to stdout so the parent
    // can split init vs staging time.
    if let Some(Cmd::ExecWorkload {
        name,
        dest,
        verbose,
    }) = &cli.cmd
    {
        let workload =
            workloads::by_name(name).with_context(|| format!("unknown workload: {name}"))?;
        std::fs::create_dir_all(dest)?;
        println!("{}", backend::READY_MARKER);
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
        println!("{}", backend::READY_MARKER);
        workload.run(&dest, *verbose)?;
        return Ok(());
    }

    let env = collect_env();

    if let Some(Cmd::Profile {
        workload: wname,
        scenario: sname,
        no_bpftrace,
    }) = cli.cmd
    {
        let scenarios: Vec<&str> = match sname.as_deref() {
            Some(s) => vec![s],
            None => vec!["agfs-allow-all", "agfs-realistic"],
        };
        for sname in scenarios {
            run_profile(&env, &wname, sname, !no_bpftrace)?;
        }
        return Ok(());
    }

    if let Some(Cmd::List) = cli.cmd {
        println!("Workloads:");
        for w in workloads::all() {
            println!("  {}", w.name());
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

    if let Some(Cmd::Rerender) = cli.cmd {
        let out_dir = results_dir(&env.hostname, false);
        let results_path = out_dir.join("results.json");
        let json = fs::read_to_string(&results_path)
            .with_context(|| format!("reading {}", results_path.display()))?;
        let results: BenchResults = serde_json::from_str(&json).context("parsing results.json")?;
        report::render(&results, &out_dir)?;
        return Ok(());
    }

    let selected_workloads: Vec<Box<dyn Workload>> = if let Some(name) = &cli.workload {
        let w = workloads::by_name(name).with_context(|| format!("unknown workload: {name}"))?;
        vec![w]
    } else {
        workloads::all()
    };

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

    let mut workload_results = Vec::new();

    for workload in &selected_workloads {
        eprintln!("Running workload: {}", workload.name());
        workload.ensure_fixture()?;

        // Warm up with native to populate page cache / dentry cache.
        eprintln!("  warm-up…");
        {
            let native = backends::native::Native;
            native
                .run_one(workload.as_ref(), cli.verbose)
                .context("warm-up iteration failed")?;
        }

        let mut backend_results = Vec::new();
        for b in &selected_backends {
            let result = run_backend(b.as_ref(), workload.as_ref(), cli.verbose, cli.runs)?;
            backend_results.push(result);
        }

        workload_results.push(WorkloadResult {
            workload: workload.name().to_string(),
            backends: backend_results,
        });
    }

    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    let results = BenchResults {
        timestamp,
        env: env.clone(),
        workloads: workload_results,
    };

    let out_dir = results_dir(&env.hostname, cli.timestamped_results);
    let merged = write_results(&results, &out_dir)?;
    report::render(&merged, &out_dir)?;

    Ok(())
}

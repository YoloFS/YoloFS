// agfs-bench — benchmark suite for the agfs filesystem.
//
// Usage:
//   agfs-bench [--workload <name>] [--scenario <name>] [--verbose] [--timestamped-results]
//   agfs-bench rerender

mod report;
mod workload;
mod workloads;

use agfs::klog;
use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Instant, SystemTime, UNIX_EPOCH};
#[allow(unused_imports)]
use workload::{IterResult, Workload};

// ── Constants ─────────────────────────────────────────────────────────────────

/// Timed iterations per (workload, scenario). One additional warm-up run
/// precedes these; its result is discarded.
const N_ITERS: usize = 3;

// ── CLI ───────────────────────────────────────────────────────────────────────

#[derive(Parser)]
#[command(name = "agfs-bench", about = "agfs benchmark suite")]
struct Cli {
    #[command(subcommand)]
    cmd: Option<Cmd>,

    /// Run only this workload
    #[arg(long)]
    workload: Option<String>,

    /// Run only this scenario
    #[arg(long)]
    scenario: Option<String>,

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
    /// List available workloads
    List,
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
struct ScenarioResult {
    scenario: String,
    iterations: Vec<IterResult>,
    mean_total_ms: f64,
    stddev_total_ms: f64,
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
    scenarios: Vec<ScenarioResult>,
}

#[derive(Serialize, Deserialize, Clone)]
struct BenchResults {
    timestamp: u64,
    env: Env,
    workloads: Vec<WorkloadResult>,
}

// ── Scenarios ─────────────────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq, Eq)]
enum Scenario {
    Native,
    RulesAllowAll,
    RulesRealistic,
}

impl Scenario {
    fn all() -> &'static [Scenario] {
        use Scenario::*;
        &[Native, RulesAllowAll, RulesRealistic]
    }

    fn name(self) -> &'static str {
        match self {
            Scenario::Native => "native",
            Scenario::RulesAllowAll => "rules-allow-all",
            Scenario::RulesRealistic => "rules-realistic",
        }
    }

    fn from_name(s: &str) -> Option<Scenario> {
        Scenario::all().iter().copied().find(|sc| sc.name() == s)
    }

    fn uses_agfs(self) -> bool {
        self != Scenario::Native
    }
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

// ── Session management ────────────────────────────────────────────────────────

struct Session {
    root: tempfile::TempDir,
    scenario: Scenario,
}

impl Session {
    fn setup(scenario: Scenario, workload: &dyn Workload) -> Result<Self> {
        let hint = dirs_next::cache_dir()
            .unwrap_or_else(|| PathBuf::from("/tmp"))
            .join("agfs-bench");
        fs::create_dir_all(&hint).context("creating agfs-bench cache dir")?;

        let root = tempfile::Builder::new()
            .prefix("agfs-bench-")
            .tempdir_in(&hint)
            .context("creating session tempdir")?;

        if !scenario.uses_agfs() {
            return Ok(Session { root, scenario });
        }

        let config = make_config(scenario, root.path(), workload);
        config
            .save(&root.path().join("agfs.toml"))
            .context("writing agfs.toml")?;

        let out = Command::new("agfs")
            .arg("mount")
            .current_dir(root.path())
            .env("NO_COLOR", "1")
            .output()
            .context("running agfs mount")?;
        if !out.status.success() {
            bail!(
                "agfs mount failed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        }

        Ok(Session { root, scenario })
    }

    fn mnt_path(&self, rel: &str) -> PathBuf {
        let root = self.root.path();
        root.join(".agfs/mnt")
            .join(root.strip_prefix("/").unwrap_or(root))
            .join(rel)
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        if self.scenario.uses_agfs() {
            let _ = Command::new("agfs")
                .arg("unmount")
                .current_dir(self.root.path())
                .env("NO_COLOR", "1")
                .output();
        }
    }
}

fn make_config(
    scenario: Scenario,
    session_root: &Path,
    workload: &dyn Workload,
) -> agfs::config::Config {
    use agfs::config::{Config, MountConfig, Perm};
    let mut rules = BTreeMap::new();
    match scenario {
        Scenario::RulesAllowAll => {
            rules.insert("/".to_string(), Perm::AllowRw);
        }
        Scenario::RulesRealistic => {
            for (path, perm) in workload.realistic_rules(session_root) {
                rules.insert(path, perm);
            }
        }
        Scenario::Native => {}
    };
    Config {
        mount: MountConfig { noperm: true, ..Default::default() },
        rules,
    }
}

// ── Generic iteration runner ──────────────────────────────────────────────────

fn run_iteration(
    session: &Session,
    workload: &dyn Workload,
    verbose: bool,
) -> Result<(IterResult, Vec<String>)> {
    let cursor = klog::snapshot();

    let dest = if session.scenario.uses_agfs() {
        session.mnt_path(workload.work_dir())
    } else {
        session.root.path().join(workload.work_dir())
    };

    // Time the staged work.
    let t0 = Instant::now();
    workload.run(&dest, verbose)?;
    let staging_ms = t0.elapsed().as_millis() as u64;

    let result = if session.scenario.uses_agfs() {
        let t1 = Instant::now();
        let out = Command::new("agfs")
            .arg("commit")
            .current_dir(session.root.path())
            .env("NO_COLOR", "1")
            .stdout(if verbose {
                Stdio::inherit()
            } else {
                Stdio::null()
            })
            .output()
            .context("running agfs commit")?;
        let commit_ms = t1.elapsed().as_millis() as u64;
        if !out.status.success() {
            bail!(
                "agfs commit failed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        }
        IterResult {
            staging_ms: Some(staging_ms),
            commit_ms: Some(commit_ms),
            total_ms: staging_ms + commit_ms,
        }
    } else {
        IterResult {
            staging_ms: None,
            commit_ms: None,
            total_ms: staging_ms,
        }
    };

    let kernel_msgs = cursor.as_deref().map(klog::since).unwrap_or_default();

    // No explicit cleanup: the session owns a TempDir that is deleted on drop,
    // giving the next iteration a completely fresh base directory and mount.

    Ok((result, kernel_msgs))
}

fn read_agfs_journal_debug(session: &Session) -> String {
    if !session.scenario.uses_agfs() {
        return String::new();
    }
    let agfs_dir = session.root.path().join(".agfs");
    match agfs::journal::read(&agfs_dir) {
        Ok(records) => records
            .iter()
            .map(|r| format!("  {r:?}"))
            .collect::<Vec<_>>()
            .join("\n"),
        Err(_) => String::new(),
    }
}

// ── Scenario runner ───────────────────────────────────────────────────────────

fn run_scenario(
    scenario: Scenario,
    workload: &dyn Workload,
    verbose: bool,
) -> Result<ScenarioResult> {
    eprintln!("  scenario: {}", scenario.name());

    let mut iterations = Vec::with_capacity(N_ITERS);
    let mut all_kernel_msgs: Vec<String> = Vec::new();
    for i in 0..N_ITERS {
        eprintln!("    iter {}/{}…", i + 1, N_ITERS);
        let session = Session::setup(scenario, workload)?;
        let (result, kernel_msgs) = match run_iteration(&session, workload, verbose) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("    iter {} failed: {e:#}", i + 1);
                let journal = read_agfs_journal_debug(&session);
                if !journal.is_empty() {
                    eprintln!("    agfs journal at failure:\n{journal}");
                }
                eprintln!("    rerunning with verbose logging…");
                run_iteration(&session, workload, true)
                    .with_context(|| format!("iter {} (verbose rerun) failed", i + 1))?
            }
        };
        if !kernel_msgs.is_empty() || verbose {
            all_kernel_msgs.extend(kernel_msgs);
        }
        iterations.push(result);
        // session drops here → unmount + tempdir deleted
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

    Ok(ScenarioResult {
        scenario: scenario.name().to_string(),
        iterations,
        mean_total_ms: stats.mean_total,
        stddev_total_ms: stats.stddev_total,
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
    mean_staging: Option<f64>,
    mean_commit: Option<f64>,
    outlier_iter_indices: Vec<usize>,
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

    let mean_staging = if iters[0].staging_ms.is_some() {
        Some(
            iters
                .iter()
                .map(|r| r.staging_ms.unwrap() as f64)
                .sum::<f64>()
                / n,
        )
    } else {
        None
    };

    let mean_commit = if iters[0].commit_ms.is_some() {
        Some(
            iters
                .iter()
                .map(|r| r.commit_ms.unwrap() as f64)
                .sum::<f64>()
                / n,
        )
    } else {
        None
    };

    Stats {
        mean_total,
        stddev_total,
        mean_staging,
        mean_commit,
        outlier_iter_indices,
    }
}

// ── Output ────────────────────────────────────────────────────────────────────

fn results_dir(hostname: &str, timestamped: bool) -> PathBuf {
    let base = PathBuf::from("bench-results").join(hostname);
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

fn write_results(results: &BenchResults, out_dir: &Path) -> Result<()> {
    fs::create_dir_all(out_dir).context("creating results dir")?;
    let json_path = out_dir.join("results.json");

    // Merge with existing results: replace workload entries that were re-run,
    // preserve workload entries that were not part of this run.
    let merged = if json_path.exists() {
        let existing: BenchResults = serde_json::from_str(
            &fs::read_to_string(&json_path).context("reading existing results.json")?,
        )
        .context("parsing existing results.json")?;

        let new_names: std::collections::HashSet<&str> = results
            .workloads
            .iter()
            .map(|w| w.workload.as_str())
            .collect();

        let mut workloads: Vec<WorkloadResult> = existing
            .workloads
            .into_iter()
            .filter(|w| !new_names.contains(w.workload.as_str()))
            .collect();
        workloads.extend(results.workloads.iter().cloned());
        workloads.sort_by(|a, b| a.workload.cmp(&b.workload));

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
    Ok(())
}

// ── Main ──────────────────────────────────────────────────────────────────────

fn main() -> Result<()> {
    let cli = Cli::parse();

    let env = collect_env();

    if let Some(Cmd::List) = cli.cmd {
        for w in workloads::all() {
            println!("{}", w.name());
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

    let scenarios: Vec<Scenario> = if let Some(name) = &cli.scenario {
        let sc = Scenario::from_name(name).with_context(|| format!("unknown scenario: {name}"))?;
        vec![sc]
    } else {
        Scenario::all().to_vec()
    };

    let mut workload_results = Vec::new();

    for workload in &selected_workloads {
        eprintln!("Running workload: {}", workload.name());
        workload.ensure_fixture()?;

        eprintln!("  warm-up…");
        {
            let session = Session::setup(Scenario::Native, workload.as_ref())?;
            run_iteration(&session, workload.as_ref(), cli.verbose)
                .context("warm-up iteration failed")?;
        }

        let mut scenario_results = Vec::new();
        for &sc in &scenarios {
            let result = run_scenario(sc, workload.as_ref(), cli.verbose)?;
            scenario_results.push(result);
        }

        workload_results.push(WorkloadResult {
            workload: workload.name().to_string(),
            scenarios: scenario_results,
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
    write_results(&results, &out_dir)?;
    report::render(&results, &out_dir)?;

    Ok(())
}

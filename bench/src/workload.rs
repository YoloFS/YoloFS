// Workload trait and shared iteration result type.

use agfs::config::Perm;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Timing for one timed iteration.
///
/// Phases: `init` (mount/checkpoint), `staging` (workload), `commit` (apply).
/// Any phase may be None if the backend doesn't have that step (e.g. native
/// has no phases).
///
/// For Op workloads, `op_result` carries self-reported metrics; the phase
/// timings are not meaningful.
#[derive(Serialize, Deserialize, Clone)]
pub struct IterResult {
    pub init_ms: Option<u64>,
    pub staging_ms: Option<u64>,
    /// Time to query staged changes in microseconds (agfs status, overlayfs upper walk, etc.)
    #[serde(skip_serializing_if = "Option::is_none", alias = "status_ms")]
    pub status_us: Option<u64>,
    pub commit_ms: Option<u64>,
    pub total_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub op_result: Option<OpResult>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub checkpoint_series: Option<CheckpointLatencySeries>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub macro_step_series: Option<MacroStepSeries>,
}

/// Per-checkpoint latency samples for checkpoint-scaling workloads.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct CheckpointLatencySeries {
    pub points: Vec<CheckpointLatencyPoint>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct CheckpointLatencyPoint {
    pub checkpoint: u32,
    pub stat_avg_lat_us: f64,
    pub readdir_avg_lat_us: f64,
    pub unlink_avg_lat_us: f64,
    pub read_avg_lat_us: f64,
    pub create_avg_lat_us: f64,
    pub overwrite_avg_lat_us: f64,
    pub file_count: usize,
    pub checkpoint_ms: u64,
}

/// Ordered per-step timings for macro workloads that report internal phases.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct MacroStepSeries {
    pub steps: Vec<MacroStepTiming>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct MacroStepTiming {
    pub step: String,
    pub ms: u64,
}

/// Per-operation metrics reported by Op workloads.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct OpResult {
    pub iops: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub throughput_kbps: Option<u64>,
    #[serde(default)]
    pub lat_us_mean: f64,
    pub lat_us_p50: f64,
    pub lat_us_p99: f64,
    pub lat_us_p999: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub read_avg_lat_us: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub write_avg_lat_us: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub read_lat_us_p50: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub read_lat_us_p99: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub write_lat_us_p50: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub write_lat_us_p99: Option<f64>,
}

/// Workload category.
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum WorkloadKind {
    Micro,
    Macro,
    Op,
}

/// Cache handling requested by a workload before it starts running.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum CacheMode {
    Default,
    DropPageCache,
}

/// A self-contained benchmark workload.
///
/// Each workload defines what it needs (fixture, rules) and what it does
/// (`run`). The runner handles all backend mechanics (mount, commit, etc).
pub trait Workload: Send + Sync {
    /// Short identifier used on the CLI and in results JSON.
    fn name(&self) -> &'static str;

    /// Micro (isolated operation) or macro (real-world task).
    fn kind(&self) -> WorkloadKind;

    /// One-line description shown as a tooltip in reports.
    fn description(&self) -> &'static str;

    /// Subdirectory within the session root where the workload operates.
    /// Must be stable across iterations.
    fn work_dir(&self) -> &'static str;

    /// Ensure any external fixtures exist (e.g. download a source repo).
    /// Must be idempotent; called once before any scenarios run.
    fn ensure_fixture(&self) -> Result<()>;

    /// Rules to apply for the `rules-realistic` scenario.
    /// Returns (path, perm) pairs meaningful for this workload.
    fn realistic_rules(&self, session_root: &Path) -> Vec<(String, Perm)>;

    /// Populate the base directory before the backend mounts over it.
    /// Called once per iteration with the raw (unmounted) base path.
    /// Default: no-op. Workloads that need pre-existing files (read, stat,
    /// overwrite, rename) override this to create them in the base layer so
    /// that backends exercise copy-up / passthrough correctly.
    fn populate_base(&self, _base_work_dir: &Path) -> Result<()> {
        Ok(())
    }

    /// Prepare the mounted work directory before the timed workload starts.
    /// This runs after the backend has created and mounted `dest`, but before
    /// any requested cache drop. Op workloads with sandbox-local fixtures
    /// (such as fio seed files) use this so setup work is not folded into the
    /// timed run while still living inside the sandboxed view of the backend.
    fn prepare_workdir(&self, _dest: &Path) -> Result<()> {
        Ok(())
    }

    /// Whether this workload needs `prepare_workdir()` to be called.
    /// Backends use this to avoid extra setup work when the default no-op
    /// implementation is sufficient.
    fn needs_prepare_workdir(&self) -> bool {
        false
    }

    /// Cache preparation required before the workload subprocess starts.
    /// Cold-cache workloads use this so the backend can drop caches from the
    /// parent process even when the workload itself runs inside a user
    /// namespace.
    fn cache_mode(&self) -> CacheMode {
        CacheMode::Default
    }

    /// Whether the backend should take a checkpoint between `prepare_workdir()`
    /// and the timed workload run. Only checkpoint-source metadata variants
    /// return true; this tells the backend to snapshot the staging state so
    /// that subsequent operations exercise the re-COW / copy-up path.
    fn needs_checkpoint(&self) -> bool {
        false
    }

    /// Whether this workload should be hidden from default listing.
    /// Hidden workloads are still runnable but not shown unless explicitly
    /// requested.
    fn hidden(&self) -> bool {
        false
    }

    /// Perform the workload. `dest` is the target path — inside the agfs mount
    /// for agfs scenarios, or a direct base path for native.
    ///
    /// For Op workloads, `run` is responsible for printing the results marker
    /// and JSON to stdout (see `backend::RESULTS_MARKER`).
    fn run(&self, dest: &Path, verbose: bool) -> Result<()>;
}

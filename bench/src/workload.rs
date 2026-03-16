// Workload trait and shared iteration result type.

use agfs::config::Perm;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Timing for one timed iteration.
///
/// Phases: `init` (mount/checkpoint), `staging` (workload), `commit` (apply).
/// Any phase may be None if the backend doesn't have that step (e.g. native
/// has no phases, try has no separable init).
#[derive(Serialize, Deserialize, Clone)]
pub struct IterResult {
    pub init_ms: Option<u64>,
    pub staging_ms: Option<u64>,
    pub commit_ms: Option<u64>,
    pub total_ms: u64,
}

/// Whether a workload is a focused micro-operation or a real-world macro task.
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum WorkloadKind {
    Micro,
    Macro,
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

    /// Perform the workload. `dest` is the target path — inside the agfs mount
    /// for agfs scenarios, or a direct base path for native.
    /// Should not call `agfs commit`; the runner handles that.
    fn run(&self, dest: &Path, verbose: bool) -> Result<()>;
}

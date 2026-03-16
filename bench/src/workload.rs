// Workload trait and shared iteration result type.

use agfs::config::Perm;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Timing for one timed iteration.
///
/// Phases: `init` (mount/snapshot), `staging` (workload), `commit` (apply).
/// Any phase may be None if the backend doesn't have that step (e.g. native
/// has no phases, try has no separable init).
#[derive(Serialize, Deserialize, Clone)]
pub struct IterResult {
    pub init_ms: Option<u64>,
    pub staging_ms: Option<u64>,
    pub commit_ms: Option<u64>,
    pub total_ms: u64,
}

/// A self-contained benchmark workload.
///
/// Each workload defines what it needs (fixture, rules) and what it does
/// (`run`). The runner handles all agfs mechanics (mount, commit, kmsg).
pub trait Workload: Send + Sync {
    /// Short identifier used on the CLI and in results JSON.
    fn name(&self) -> &'static str;

    /// Subdirectory within the session root where the workload operates.
    /// Must be stable across iterations.
    fn work_dir(&self) -> &'static str;

    /// Ensure any external fixtures exist (e.g. download a source repo).
    /// Must be idempotent; called once before any scenarios run.
    fn ensure_fixture(&self) -> Result<()>;

    /// Rules to apply for the `rules-realistic` scenario.
    /// Returns (path, perm) pairs meaningful for this workload.
    fn realistic_rules(&self, session_root: &Path) -> Vec<(String, Perm)>;

    /// Perform the workload. `dest` is the target path — inside the agfs mount
    /// for agfs scenarios, or a direct base path for native.
    /// Should not call `agfs commit`; the runner handles that.
    fn run(&self, dest: &Path, verbose: bool) -> Result<()>;
}

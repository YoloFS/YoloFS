// Backend trait — abstraction over staging/commit mechanisms.

use crate::workload::{IterResult, Workload};
use anyhow::{Context, Result, bail};
use std::io::BufRead;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Instant;

/// Marker printed by `exec-workload` to stdout right before the workload runs.
pub const READY_MARKER: &str = "AGFS_BENCH_READY";

/// A backend defines how writes are staged and committed.
///
/// Each implementation manages its own session lifecycle (mount, run, commit,
/// unmount/teardown) within `run_one`.
pub trait Backend: Send + Sync {
    /// Short identifier used on the CLI and in results JSON.
    fn name(&self) -> &'static str;

    /// Returns false if required external tools are absent.
    fn available(&self) -> bool {
        true
    }

    /// Human-readable reason why the backend is unavailable.
    fn unavailable_reason(&self) -> Option<&'static str> {
        None
    }

    /// If true, this backend is excluded from default runs and only included
    /// when explicitly requested via `--backend`.
    fn hidden(&self) -> bool {
        false
    }

    /// Run one timed iteration: set up, run workload, commit, tear down.
    /// Returns (timing, kernel_messages).
    fn run_one(&self, workload: &dyn Workload, verbose: bool) -> Result<(IterResult, Vec<String>)>;
}

/// Result of spawning a workload subprocess via [`run_workload_subprocess`].
pub struct SubprocessResult {
    /// Wall time from spawn to READY marker (process startup + any
    /// namespace/overlay setup that wraps the subprocess).
    pub startup_ms: u64,
    /// Wall time from READY marker to process exit (the workload itself).
    pub staging_ms: u64,
}

/// Build the base `Command` for `exec-workload`. Backends that run the
/// workload directly (native, agfs, branchfs) use this as-is. The `try`
/// backend wraps it inside `try -n -D <sandbox> -- ...`.
pub fn exec_workload_cmd(workload_name: &str, dest: &Path, verbose: bool) -> Result<Command> {
    let self_exe = std::env::current_exe().context("resolving current executable")?;
    let mut cmd = Command::new(self_exe);
    cmd.arg("exec-workload")
        .arg("--name")
        .arg(workload_name)
        .arg("--dest")
        .arg(dest);
    if verbose {
        cmd.arg("--verbose");
    }
    Ok(cmd)
}

/// Spawn a workload subprocess, wait for the READY marker on stdout, then
/// wait for exit. Returns startup time (spawn → READY) and staging time
/// (READY → exit).
///
/// The caller provides a fully configured `Command` — it may be a bare
/// `exec-workload` invocation or one wrapped by `try`.
pub fn run_workload_subprocess(cmd: &mut Command) -> Result<SubprocessResult> {
    cmd.stdout(Stdio::piped());
    // stderr: leave as-is (caller may have set inherit or piped)

    let t0 = Instant::now();
    let mut child = cmd.spawn().context("spawning workload subprocess")?;

    let stdout = child.stdout.take().unwrap();
    let reader = std::io::BufReader::new(stdout);

    let mut got_ready = false;
    for line in reader.lines() {
        let line = line.context("reading workload subprocess stdout")?;
        if line.trim() == READY_MARKER {
            got_ready = true;
            break;
        }
    }

    let startup_ms = t0.elapsed().as_millis() as u64;

    if !got_ready {
        let output = child.wait_with_output()?;
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("workload subprocess exited without READY marker: {stderr}");
    }

    let output = child
        .wait_with_output()
        .context("waiting for workload subprocess")?;
    let total_ms = t0.elapsed().as_millis() as u64;
    let staging_ms = total_ms - startup_ms;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("workload subprocess failed: {stderr}");
    }

    Ok(SubprocessResult {
        startup_ms,
        staging_ms,
    })
}

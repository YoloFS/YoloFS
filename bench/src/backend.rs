// Backend trait — abstraction over staging/commit mechanisms.

use crate::workload::{CheckpointLatencySeries, IterResult, MacroStepSeries, OpResult, Workload};
use anyhow::{Context, Result, bail};
use std::io::BufRead;
use std::io::Write;
use std::path::Path;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::time::Instant;

/// Marker printed by `exec-workload` to stdout right before the workload runs.
pub const READY_MARKER: &str = "YOLO_BENCH_READY";

/// Marker printed by Op workloads to stdout after the workload finishes,
/// followed by a JSON line containing the `OpResult`.
pub const RESULTS_MARKER: &str = "YOLO_BENCH_RESULTS";

/// Marker printed by workloads that need a backend-managed checkpoint.
/// The next line is a JSON object with `step`.
pub const CHECKPOINT_MARKER: &str = "YOLO_BENCH_CHECKPOINT";

#[derive(serde::Deserialize)]
struct CheckpointRequest {
    step: usize,
}

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

    /// Check whether this backend can meaningfully run the given workload.
    /// Returns `Some(reason)` if the combination is invalid and should be
    /// skipped, `None` if supported.
    fn unsupported_reason(&self, _workload: &dyn Workload) -> Option<&'static str> {
        None
    }

    /// Like `unsupported_reason`, but only checked when the backend was
    /// selected implicitly (no explicit `--backend`). Returns `Some(reason)`
    /// to skip this backend for the given workload by default while still
    /// allowing it when explicitly requested.
    fn default_skip_reason(&self, _workload: &dyn Workload) -> Option<&'static str> {
        None
    }

    /// Run one timed iteration: set up, run workload, commit, tear down.
    /// Returns (timing, kernel_messages).
    fn run_one(&self, workload: &dyn Workload, verbose: bool) -> Result<(IterResult, Vec<String>)>;
}

/// Result of spawning a workload subprocess via [`run_workload_subprocess`].
pub struct SubprocessResult {
    /// Wall time from spawn to READY marker (process startup + any
    /// namespace/overlay setup that wraps the subprocess).
    #[allow(dead_code)]
    pub startup_ms: u64,
    /// Wall time from READY marker to process exit (the workload itself).
    pub staging_ms: u64,
    /// Self-reported op metrics, if the subprocess printed RESULTS + JSON.
    pub op_result: Option<OpResult>,
    /// Self-reported checkpoint-series metrics for checkpoint-scaling workloads.
    pub checkpoint_series: Option<CheckpointLatencySeries>,
    /// Self-reported per-step timings for macro workloads.
    pub macro_step_series: Option<MacroStepSeries>,
}

pub trait CheckpointController {
    fn checkpoint(&mut self, step: usize) -> Result<CheckpointOutcome>;
}

pub enum CheckpointOutcome {
    Continue {
        checkpoint_ms: u64,
        /// If set, the workload subprocess should chdir to this path.
        next_dest: Option<std::path::PathBuf>,
    },
    Stop,
}

pub struct NoopCheckpointController;

impl CheckpointController for NoopCheckpointController {
    fn checkpoint(&mut self, _step: usize) -> Result<CheckpointOutcome> {
        Ok(CheckpointOutcome::Continue {
            checkpoint_ms: 0,
            next_dest: None,
        })
    }
}

/// Build the base `Command` for `exec-workload`. Backends that run the
/// workload directly (native, yolo, overlayfs, branchfs) use this as-is.
pub fn exec_workload_cmd(
    workload_name: &str,
    dest: &Path,
    verbose: bool,
    wait_after_ready: bool,
) -> Result<Command> {
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
    if wait_after_ready {
        cmd.arg("--wait-after-ready");
    }
    Ok(cmd)
}

// ── Two-phase subprocess API ─────────────────────────────────────────────────
//
// Backends that need to do work between READY and GO (e.g. unmount+remount
// for cold metadata benchmarks) use `spawn_and_await_ready` + `PausedSubprocess::go`.
// Backends that just need a simple drop_caches use the convenience wrapper
// `run_workload_subprocess`.

/// A subprocess that has printed READY and is paused, waiting for the
/// parent to send GO on stdin.
pub struct PausedSubprocess {
    child: Child,
    stdin: ChildStdin,
    reader: std::io::BufReader<ChildStdout>,
    pub startup_ms: u64,
    t0: Instant,
    wait_after_ready: bool,
}

/// Spawn a workload subprocess and wait for it to print the READY marker.
///
/// If `needs_signal` is true, stdin is piped so the caller can do work
/// (cache drops, unmount/remount) before calling [`PausedSubprocess::go`].
pub fn spawn_and_await_ready(cmd: &mut Command, needs_signal: bool) -> Result<PausedSubprocess> {
    cmd.stdout(Stdio::piped());
    cmd.stdin(Stdio::piped());

    let t0 = Instant::now();
    let mut child = cmd.spawn().context("spawning workload subprocess")?;

    let stdout = child.stdout.take().unwrap();
    let stdin = child
        .stdin
        .take()
        .context("capturing workload subprocess stdin")?;
    let mut reader = std::io::BufReader::new(stdout);

    let mut got_ready = false;
    let mut line_buf = String::new();
    loop {
        line_buf.clear();
        let n = reader
            .read_line(&mut line_buf)
            .context("reading workload subprocess stdout")?;
        if n == 0 {
            break;
        }
        if line_buf.trim() == READY_MARKER {
            got_ready = true;
            break;
        }
    }

    if !got_ready {
        let output = child.wait_with_output()?;
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("workload subprocess exited without READY marker: {stderr}");
    }

    let startup_ms = t0.elapsed().as_millis() as u64;
    Ok(PausedSubprocess {
        child,
        stdin,
        reader,
        startup_ms,
        t0,
        wait_after_ready: needs_signal,
    })
}

impl PausedSubprocess {
    /// Signal the subprocess to proceed, then wait for exit and parse results.
    pub fn go(self) -> Result<SubprocessResult> {
        let mut noop = NoopCheckpointController;
        self.go_with_checkpoint(&mut noop)
    }

    pub fn go_with_checkpoint(
        mut self,
        cp: &mut dyn CheckpointController,
    ) -> Result<SubprocessResult> {
        if self.wait_after_ready {
            writeln!(self.stdin, "GO").context("signalling subprocess to proceed")?;
        }

        let mut op_result = None;
        let mut checkpoint_series = None;
        let mut macro_step_series = None;
        let mut found_results = false;
        let mut expect_checkpoint_json = false;
        let mut line_buf = String::new();
        loop {
            line_buf.clear();
            let n = self
                .reader
                .read_line(&mut line_buf)
                .context("reading workload subprocess stdout")?;
            if n == 0 {
                break;
            }
            let trimmed = line_buf.trim();
            if expect_checkpoint_json {
                let req: CheckpointRequest = serde_json::from_str(trimmed)
                    .with_context(|| format!("parsing checkpoint request JSON: {trimmed}"))?;
                let response = cp.checkpoint(req.step)?;
                writeln!(self.stdin, "{}", RESULTS_MARKER)
                    .context("writing checkpoint response marker")?;
                match response {
                    CheckpointOutcome::Continue {
                        checkpoint_ms,
                        next_dest,
                    } => {
                        let dest_json = match &next_dest {
                            Some(p) => format!("\"{}\"", p.display()),
                            None => "null".to_string(),
                        };
                        writeln!(
                            self.stdin,
                            "{{\"checkpoint_ms\":{checkpoint_ms},\"stop\":false,\"next_dest\":{dest_json}}}"
                        )
                        .context("writing checkpoint response json")?;
                    }
                    CheckpointOutcome::Stop => {
                        writeln!(self.stdin, "{{\"checkpoint_ms\":0,\"stop\":true}}")
                            .context("writing checkpoint response json")?;
                    }
                }
                expect_checkpoint_json = false;
                continue;
            }
            if trimmed == CHECKPOINT_MARKER {
                expect_checkpoint_json = true;
                continue;
            }
            if trimmed == RESULTS_MARKER {
                found_results = true;
                continue;
            }
            if found_results {
                if let Ok(op) = serde_json::from_str::<OpResult>(trimmed) {
                    op_result = Some(op);
                } else if let Ok(series) = serde_json::from_str::<CheckpointLatencySeries>(trimmed)
                {
                    checkpoint_series = Some(series);
                } else if let Ok(series) = serde_json::from_str::<MacroStepSeries>(trimmed) {
                    macro_step_series = Some(series);
                } else {
                    bail!("parsing workload result JSON failed: {trimmed}");
                }
            }
        }

        let status = self
            .child
            .wait()
            .context("waiting for workload subprocess")?;
        let total_ms = self.t0.elapsed().as_millis() as u64;
        let staging_ms = total_ms - self.startup_ms;

        if !status.success() {
            bail!("workload subprocess failed (exit {})", status);
        }

        Ok(SubprocessResult {
            startup_ms: self.startup_ms,
            staging_ms,
            op_result,
            checkpoint_series,
            macro_step_series,
        })
    }
}

/// Convenience: spawn, optionally drop page caches after READY, then finish.
///
/// For backends that need to do more between READY and GO (e.g. unmount +
/// remount), use [`spawn_and_await_ready`] + [`PausedSubprocess::go`] directly.
pub fn run_workload_subprocess(
    cmd: &mut Command,
    drop_caches_after_ready: bool,
) -> Result<SubprocessResult> {
    let sp = spawn_and_await_ready(cmd, drop_caches_after_ready)?;
    if drop_caches_after_ready {
        crate::workloads::drop_page_cache()
            .context("dropping page cache after subprocess READY")?;
    }
    sp.go()
}

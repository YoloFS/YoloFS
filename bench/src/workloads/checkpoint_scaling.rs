// checkpoint-scaling: measure op latency as a function of checkpoint depth.
//
// Four modes (set via CHECKPOINT_SCALING_MODE env var):
// - "create": after K checkpoints, measure creating 100 new files
// - "read": after K checkpoints, measure reading 100 files created before any checkpoint
// - "commit": build K checkpoints, then exit (commit time measured by backend)
// - "status": build K checkpoints, then exit (status time measured by backend)
//
// CHECKPOINT_SCALING_DEPTH sets K (number of checkpoints to create first).
//
// Before any checkpoints, 100 target files and 10 seed files are created.
// Checkpoint creation is intentionally trivial: each checkpoint just overwrites
// the 10 seed files. This isolates the effect of checkpoint *depth* on the
// measured operations without inflating per-layer state.

use crate::workload::{Workload, WorkloadKind};
use anyhow::{Context, Result};
use std::path::Path;
use std::time::Instant;
use yolofs::config::Perm;

const MEASURE_FILES: usize = 100;
const SEED_FILES: usize = 10;

pub struct CheckpointScaling;

pub fn details() -> crate::workloads::WorkloadDetails {
    crate::workloads::workload_details(
        "Checkpoint-depth scaling workload (fixed-size create/read probes).",
        "Creates synthetic files under a single workload directory; no external fixture required.",
        Some(
            "Uses backend checkpoint protocol between setup phases. Configure with CHECKPOINT_SCALING_MODE={create|read|commit|status} and CHECKPOINT_SCALING_DEPTH=<N>. Checkpoint creation is trivial: 10 seed files are overwritten at each layer.",
        ),
        "Builds a checkpoint chain (overwriting 10 seed files per layer), then measures 100 create or 100 read operations (or exits for commit-time measurement) and emits OpResult.",
        file!(),
    )
}

impl Workload for CheckpointScaling {
    fn name(&self) -> &'static str {
        "checkpoint-scaling"
    }

    fn kind(&self) -> WorkloadKind {
        WorkloadKind::Micro
    }

    fn description(&self) -> &'static str {
        "Checkpoint depth scaling (create/read latency vs checkpoint count)"
    }

    fn work_dir(&self) -> &'static str {
        "checkpoint-scaling"
    }

    fn ensure_fixture(&self) -> Result<()> {
        Ok(())
    }

    fn hidden(&self) -> bool {
        true
    }

    fn needs_checkpoint(&self) -> bool {
        // We manage checkpoints ourselves via the protocol.
        false
    }

    fn realistic_rules(&self, root: &Path) -> Vec<(String, Perm)> {
        vec![(root.to_string_lossy().into_owned(), Perm::AllowRw)]
    }

    fn run(&self, dest: &Path, _verbose: bool) -> Result<()> {
        let mode =
            std::env::var("CHECKPOINT_SCALING_MODE").unwrap_or_else(|_| "create".to_string());
        let depth: usize = std::env::var("CHECKPOINT_SCALING_DEPTH")
            .unwrap_or_else(|_| "1".to_string())
            .parse()
            .context("invalid CHECKPOINT_SCALING_DEPTH")?;

        std::fs::create_dir_all(dest)?;
        std::env::set_current_dir(dest)?;

        let root = Path::new(".");
        let buf = vec![0u8; 4096];

        // Pre-checkpoint setup (before any checkpoints):
        // - 100 target files (used as read targets in "read" mode)
        // - 10 seed files (overwritten at each checkpoint to make it non-empty)
        for i in 0..MEASURE_FILES {
            std::fs::write(root.join(format!("target-{i:06}.dat")), &buf)?;
        }
        for i in 0..SEED_FILES {
            std::fs::write(root.join(format!("seed-{i:06}.dat")), &buf)?;
        }

        // Build K checkpoints. Each one just overwrites the 10 seed files.
        let mut built_depth = 0usize;
        for chk in 0..depth {
            for i in 0..SEED_FILES {
                std::fs::write(root.join(format!("seed-{i:06}.dat")), &buf)?;
            }
            if !emit_checkpoint(chk + 1)? {
                break;
            }
            built_depth += 1;
        }
        eprintln!("  checkpoint-scaling({mode}): built depth {built_depth}/{depth}");
        if built_depth < depth {
            anyhow::bail!("backend stopped at depth {built_depth} (requested {depth})");
        }

        match mode.as_str() {
            "create" => {
                // Measure: create 100 new files.
                let mut latencies = Vec::with_capacity(MEASURE_FILES);
                for i in 0..MEASURE_FILES {
                    let t = Instant::now();
                    std::fs::write(root.join(format!("measure-{i:06}.dat")), &buf)?;
                    latencies.push(t.elapsed());
                }

                crate::workloads::emit_op_result(&crate::workloads::summarize_latencies(
                    latencies,
                    Instant::now().elapsed(), // not used meaningfully
                    None,
                ))?;
            }
            "read" => {
                // Measure: read the 100 target files created before checkpoints.
                let mut latencies = Vec::with_capacity(MEASURE_FILES);
                for i in 0..MEASURE_FILES {
                    let path = root.join(format!("target-{i:06}.dat"));
                    let t = Instant::now();
                    let _data = std::fs::read(&path)?;
                    latencies.push(t.elapsed());
                }

                crate::workloads::emit_op_result(&crate::workloads::summarize_latencies(
                    latencies,
                    Instant::now().elapsed(),
                    None,
                ))?;
            }
            "commit" | "status" => {
                // No measurement — just exit. The backend's commit/status
                // phase (measured by run_one) provides the timing data.
            }
            _ => anyhow::bail!("unknown CHECKPOINT_SCALING_MODE: {mode}"),
        }

        Ok(())
    }
}

fn emit_checkpoint(step: usize) -> Result<bool> {
    use std::io::{BufRead, Write};

    // Release cwd so the backend can unmount+remount (overlayfs).
    let saved_cwd = std::env::current_dir()?;
    std::env::set_current_dir("/")?;

    println!("{}", crate::backend::CHECKPOINT_MARKER);
    println!("{{\"step\":{step}}}");
    std::io::stdout().flush()?;

    let stdin = std::io::stdin();
    let mut lock = stdin.lock();
    let mut line = String::new();

    lock.read_line(&mut line)?;
    if line.trim() != crate::backend::RESULTS_MARKER {
        anyhow::bail!(
            "expected {} after checkpoint",
            crate::backend::RESULTS_MARKER
        );
    }
    line.clear();
    lock.read_line(&mut line)?;

    // Restore cwd (or use next_dest if provided).
    #[derive(serde::Deserialize)]
    struct Resp {
        #[allow(dead_code)]
        checkpoint_ms: u64,
        stop: bool,
        #[serde(default)]
        next_dest: Option<String>,
    }
    let resp: Resp = serde_json::from_str(line.trim())?;
    if resp.stop {
        return Ok(false);
    }
    let target = resp
        .next_dest
        .as_deref()
        .map(std::path::Path::new)
        .unwrap_or(&saved_cwd);
    std::env::set_current_dir(target)?;

    Ok(true)
}

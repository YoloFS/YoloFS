// checkpoint-scaling: measure op latency as a function of checkpoint depth.
//
// Two modes (set via CHECKPOINT_SCALING_MODE env var):
// - "create": after K checkpoints, measure creating 100 new files
// - "read": after K checkpoints, measure reading 100 files from checkpoint 1
//
// CHECKPOINT_SCALING_DEPTH sets K (number of checkpoints to create first).
// FILES_PER_CHECKPOINT = 100 files per checkpoint layer.

use crate::workload::{Workload, WorkloadKind};
use agfs::config::Perm;
use anyhow::{Context, Result};
use std::path::Path;
use std::time::Instant;

const FILES_PER_CHECKPOINT: usize = 100;

pub struct CheckpointScaling;

pub fn details() -> crate::workloads::WorkloadDetails {
    crate::workloads::workload_details(
        "Checkpoint-depth scaling workload (fixed-size create/read probes).",
        "Creates synthetic files under a single workload directory; no external fixture required.",
        Some(
            "Uses backend checkpoint protocol between setup phases. Configure with CHECKPOINT_SCALING_MODE={create|read} and CHECKPOINT_SCALING_DEPTH=<N>.",
        ),
        "Builds a checkpoint chain, then measures 100 create or 100 read operations and emits OpResult.",
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

        match mode.as_str() {
            "create" => {
                // Setup: create K checkpoints, each with 100 files (untimed).
                let mut file_id = 0usize;
                let mut built_depth = 0usize;
                for chk in 0..depth {
                    for _ in 0..FILES_PER_CHECKPOINT {
                        std::fs::write(root.join(format!("setup-{file_id:06}.dat")), &buf)?;
                        file_id += 1;
                    }
                    if !emit_checkpoint(chk + 1)? {
                        break;
                    }
                    built_depth += 1;
                }
                eprintln!("  checkpoint-scaling(create): built depth {built_depth}/{depth}");

                // Measure: create 100 new files.
                let mut latencies = Vec::with_capacity(FILES_PER_CHECKPOINT);
                for i in 0..FILES_PER_CHECKPOINT {
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
                // Setup: first checkpoint has the target files.
                for i in 0..FILES_PER_CHECKPOINT {
                    std::fs::write(root.join(format!("target-{i:06}.dat")), &buf)?;
                }
                if !emit_checkpoint(1)? {
                    anyhow::bail!("backend stopped at first checkpoint");
                }

                // Remaining K-1 checkpoints with filler files.
                let mut file_id = 0usize;
                let mut built_depth = 1usize;
                for chk in 1..depth {
                    for _ in 0..FILES_PER_CHECKPOINT {
                        std::fs::write(root.join(format!("filler-{file_id:06}.dat")), &buf)?;
                        file_id += 1;
                    }
                    if !emit_checkpoint(chk + 1)? {
                        break;
                    }
                    built_depth += 1;
                }
                eprintln!("  checkpoint-scaling(read): built depth {built_depth}/{depth}");

                // Measure: read the 100 target files from checkpoint 1.
                let mut latencies = Vec::with_capacity(FILES_PER_CHECKPOINT);
                for i in 0..FILES_PER_CHECKPOINT {
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

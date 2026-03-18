use crate::workload::{CheckpointLatencyPoint, CheckpointLatencySeries, Workload, WorkloadKind};
use agfs::config::Perm;
use anyhow::{Context, Result, bail};
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

const DEFAULT_CHECKPOINT_STEPS: usize = 100;
const OPS_PER_STEP: usize = 10;
const READDIR_PER_STEP: usize = 1;
const NET_GROWTH_PER_STEP: usize = 12;
const INITIAL_FILE_COUNT: usize = 128;
const OVERWRITE_BYTES: usize = 1024;

pub struct CheckpointScalability;

pub fn details() -> crate::workloads::WorkloadDetails {
    crate::workloads::workload_details(
        "Session microbenchmark for checkpoint scalability across growing checkpoint depth.",
        "Starts with 128 files and, per checkpoint, runs stat/readdir/unlink/read/create/overwrite batches with net-positive file growth.",
        Some(
            "At each checkpoint index k: run operation batches (readdir=1 op, others=10 ops), record per-op avg latency, then create a checkpoint. Set AGFS_BENCH_CHECKPOINT_STEPS to override checkpoint depth (default: 100).",
        ),
        "Runs checkpoint-scalability loop and emits per-checkpoint latency series for multiline plotting.",
        file!(),
    )
}

impl Workload for CheckpointScalability {
    fn name(&self) -> &'static str {
        "checkpoint-scalability"
    }

    fn kind(&self) -> WorkloadKind {
        WorkloadKind::Micro
    }

    fn description(&self) -> &'static str {
        "checkpoint-depth scalability (multiline per-op latency vs checkpoint)"
    }

    fn work_dir(&self) -> &'static str {
        "checkpoint-scalability"
    }

    fn ensure_fixture(&self) -> Result<()> {
        Ok(())
    }

    fn realistic_rules(&self, session_root: &Path) -> Vec<(String, Perm)> {
        vec![(session_root.to_string_lossy().into_owned(), Perm::AllowRw)]
    }

    fn run(&self, dest: &Path, _verbose: bool) -> Result<()> {
        let checkpoint_steps = checkpoint_steps()?;
        std::fs::create_dir_all(dest).with_context(|| format!("creating {}", dest.display()))?;

        let mut file_ids: Vec<usize> = Vec::new();
        for i in 0..INITIAL_FILE_COUNT {
            let p = file_path(dest, i);
            std::fs::write(&p, seed_bytes(i))
                .with_context(|| format!("creating initial fixture {}", p.display()))?;
            file_ids.push(i);
        }
        let mut next_id = INITIAL_FILE_COUNT;

        let sub = dest.join("sub");
        std::fs::create_dir_all(&sub).with_context(|| format!("creating {}", sub.display()))?;
        for i in 0..16 {
            let p = sub.join(format!("d{i:03}.dat"));
            std::fs::write(&p, seed_bytes(i))
                .with_context(|| format!("creating {}", p.display()))?;
        }

        let mut points = Vec::with_capacity(checkpoint_steps);
        for step in 1..=checkpoint_steps {
            if file_ids.len() < OPS_PER_STEP {
                bail!("not enough files for checkpoint step {step}");
            }

            let stat_ids = pick_ids(&file_ids, step * 31, OPS_PER_STEP);
            let read_ids = pick_ids(&file_ids, step * 37, OPS_PER_STEP);
            let overwrite_ids = pick_ids(&file_ids, step * 41, OPS_PER_STEP);
            let unlink_ids = pick_ids(&file_ids, step * 43, OPS_PER_STEP);

            let stat_avg = avg_us(OPS_PER_STEP, |i| {
                let p = file_path(dest, stat_ids[i]);
                let _ = std::fs::metadata(&p).with_context(|| format!("stat {}", p.display()))?;
                Ok(())
            })?;

            let readdir_avg = avg_us(READDIR_PER_STEP, |_| {
                let mut n = 0usize;
                for ent in
                    std::fs::read_dir(&sub).with_context(|| format!("readdir {}", sub.display()))?
                {
                    let _ = ent?;
                    n += 1;
                }
                if n == 0 {
                    bail!("unexpected empty subdir during readdir")
                }
                Ok(())
            })?;

            let read_avg = avg_us(OPS_PER_STEP, |i| {
                let p = file_path(dest, read_ids[i]);
                let _ = std::fs::read(&p).with_context(|| format!("read {}", p.display()))?;
                Ok(())
            })?;

            let overwrite_avg = avg_us(OPS_PER_STEP, |i| {
                let p = file_path(dest, overwrite_ids[i]);
                let mut data = vec![0u8; OVERWRITE_BYTES];
                data[0] = (step as u8).wrapping_add(i as u8);
                std::fs::write(&p, &data).with_context(|| format!("overwrite {}", p.display()))?;
                Ok(())
            })?;

            let unlink_avg = avg_us(OPS_PER_STEP, |i| {
                let id = unlink_ids[i];
                let p = file_path(dest, id);
                std::fs::remove_file(&p).with_context(|| format!("unlink {}", p.display()))?;
                Ok(())
            })?;
            file_ids.retain(|id| !unlink_ids.contains(id));

            let create_count = OPS_PER_STEP + NET_GROWTH_PER_STEP;
            let mut created = Vec::with_capacity(create_count);
            let create_avg = avg_us(create_count, |_| {
                let id = next_id;
                next_id += 1;
                let p = file_path(dest, id);
                std::fs::write(&p, seed_bytes(id))
                    .with_context(|| format!("create {}", p.display()))?;
                created.push(id);
                Ok(())
            })?;
            file_ids.extend(created);

            let checkpoint_ms = run_checkpoint_if_available(dest, step)?;

            points.push(CheckpointLatencyPoint {
                checkpoint: step as u32,
                stat_avg_lat_us: stat_avg,
                readdir_avg_lat_us: readdir_avg,
                unlink_avg_lat_us: unlink_avg,
                read_avg_lat_us: read_avg,
                create_avg_lat_us: create_avg,
                overwrite_avg_lat_us: overwrite_avg,
                file_count: file_ids.len(),
                checkpoint_ms,
            });
        }

        let series = CheckpointLatencySeries { points };
        let json = serde_json::to_string(&series).context("serializing checkpoint series")?;
        let stdout = std::io::stdout();
        let mut out = stdout.lock();
        use std::io::Write as _;
        writeln!(out, "{}", crate::backend::RESULTS_MARKER)?;
        writeln!(out, "{json}")?;
        out.flush()?;
        Ok(())
    }
}

fn checkpoint_steps() -> Result<usize> {
    match std::env::var("AGFS_BENCH_CHECKPOINT_STEPS") {
        Ok(s) => {
            let steps = s
                .parse::<usize>()
                .with_context(|| format!("invalid AGFS_BENCH_CHECKPOINT_STEPS value: {s}"))?;
            if steps == 0 {
                bail!("AGFS_BENCH_CHECKPOINT_STEPS must be >= 1");
            }
            Ok(steps)
        }
        Err(_) => Ok(DEFAULT_CHECKPOINT_STEPS),
    }
}

fn file_path(root: &Path, id: usize) -> std::path::PathBuf {
    root.join(format!("file-{id:06}.dat"))
}

fn seed_bytes(id: usize) -> Vec<u8> {
    let mut buf = vec![0u8; OVERWRITE_BYTES];
    for (i, b) in buf.iter_mut().enumerate() {
        *b = ((id * 17 + i * 13 + 7) % 251) as u8;
    }
    buf
}

fn avg_us<F>(count: usize, mut f: F) -> Result<f64>
where
    F: FnMut(usize) -> Result<()>,
{
    let mut total = Duration::ZERO;
    for i in 0..count {
        let t = Instant::now();
        f(i)?;
        total += t.elapsed();
    }
    Ok((total.as_secs_f64() * 1_000_000.0) / count as f64)
}

fn pick_ids(ids: &[usize], seed: usize, count: usize) -> Vec<usize> {
    let mut out = Vec::with_capacity(count);
    let n = ids.len();
    for i in 0..count {
        out.push(ids[(seed + i) % n]);
    }
    out
}

fn run_checkpoint_if_available(dest: &Path, step: usize) -> Result<u64> {
    let Some(session_dir) = infer_agfs_session_dir(dest) else {
        return Ok(0);
    };

    let t_chk = Instant::now();
    let chk = Command::new("agfs")
        .arg("checkpoint")
        .arg(format!("bench-step-{step:03}"))
        .env("AGFS_SESSION", &session_dir)
        .current_dir(dest)
        .output()
        .context("running agfs checkpoint")?;
    if !chk.status.success() {
        bail!(
            "agfs checkpoint failed at step {step}: {}",
            String::from_utf8_lossy(&chk.stderr)
        );
    }
    Ok(t_chk.elapsed().as_millis() as u64)
}

fn infer_agfs_session_dir(dest: &Path) -> Option<std::path::PathBuf> {
    let s = dest.to_string_lossy();
    let marker = "/.agfs/mnt/";
    let pos = s.find(marker)?;
    let root = &s[..pos];
    if root.is_empty() {
        return None;
    }
    let session = std::path::PathBuf::from(root).join(".agfs");
    session.exists().then_some(session)
}

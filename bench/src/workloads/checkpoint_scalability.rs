use crate::workload::{CheckpointLatencyPoint, CheckpointLatencySeries, Workload, WorkloadKind};
use agfs::config::Perm;
use anyhow::{bail, Context, Result};
use std::io::{BufRead as _, Write as _};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const DEFAULT_CHECKPOINT_STEPS: usize = 100;
const OPS_PER_STEP: usize = 10;
const READDIR_PER_STEP: usize = 1;
const INITIAL_FILE_COUNT: usize = 128;
const OVERWRITE_BYTES: usize = 1024;

pub struct CheckpointScalability;

pub fn details() -> crate::workloads::WorkloadDetails {
    crate::workloads::workload_details(
        "Session microbenchmark for checkpoint scalability across growing checkpoint depth.",
        "Starts with 128 files in a fixed-size directory (no growth).",
        Some(
            "Per checkpoint step: stat 10, readdir 1, read 10, overwrite 10, \
             unlink 10, create 10 (replacing the unlinked files). \
             Directory stays at 128 files throughout. \
             Set AGFS_BENCH_CHECKPOINT_STEPS to override depth (default: 100).",
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
        "Checkpoint-depth scalability: per-op latency vs checkpoint count (fixed 128-file directory)"
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
        let dest = normalize_dest(dest);
        std::fs::create_dir_all(&dest).with_context(|| format!("creating {}", dest.display()))?;
        std::env::set_current_dir(&dest)
            .with_context(|| format!("chdir to {}", dest.display()))?;

        // From here, all paths are relative to dest (cwd).
        let root = Path::new(".");

        let mut file_ids: Vec<usize> = Vec::new();
        for i in 0..INITIAL_FILE_COUNT {
            let p = file_path(root, i);
            std::fs::write(&p, seed_bytes(i))
                .with_context(|| format!("creating initial fixture {}", p.display()))?;
            file_ids.push(i);
        }
        let mut next_id = INITIAL_FILE_COUNT;

        let sub = root.join("sub");
        std::fs::create_dir_all(&sub).with_context(|| format!("creating {}", sub.display()))?;
        for i in 0..16 {
            let p = sub.join(format!("d{i:03}.dat"));
            std::fs::write(&p, seed_bytes(i))
                .with_context(|| format!("creating {}", p.display()))?;
        }

        let mut points = Vec::with_capacity(checkpoint_steps);
        eprintln!(
            "  checkpoint-scalability: {INITIAL_FILE_COUNT} files, \
             {OPS_PER_STEP} ops/step, {checkpoint_steps} steps"
        );

        for step in 1..=checkpoint_steps {
            if file_ids.len() < OPS_PER_STEP {
                bail!(
                    "not enough files ({}) for checkpoint step {step}",
                    file_ids.len()
                );
            }

            // Pick non-overlapping subsets for unlink vs other ops.
            let unlink_ids = pick_ids(&file_ids, step * 43, OPS_PER_STEP);
            let remaining: Vec<usize> = file_ids
                .iter()
                .copied()
                .filter(|id| !unlink_ids.contains(id))
                .collect();
            let stat_ids = pick_ids(&remaining, step * 31, OPS_PER_STEP);
            let read_ids = pick_ids(&remaining, step * 37, OPS_PER_STEP);
            let overwrite_ids = pick_ids(&remaining, step * 41, OPS_PER_STEP);

            // stat
            let stat_avg = avg_us(OPS_PER_STEP, |i| {
                let p = file_path(root, stat_ids[i]);
                let _ = std::fs::metadata(&p).with_context(|| format!("stat {}", p.display()))?;
                Ok(())
            })?;

            // readdir
            let readdir_avg = avg_us(READDIR_PER_STEP, |_| {
                let sub = root.join("sub");
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

            // read
            let read_avg = avg_us(OPS_PER_STEP, |i| {
                let p = file_path(root, read_ids[i]);
                let _ = std::fs::read(&p).with_context(|| format!("read {}", p.display()))?;
                Ok(())
            })?;

            // overwrite
            let overwrite_avg = avg_us(OPS_PER_STEP, |i| {
                let p = file_path(root, overwrite_ids[i]);
                let mut data = vec![0u8; OVERWRITE_BYTES];
                data[0] = (step as u8).wrapping_add(i as u8);
                std::fs::write(&p, &data).with_context(|| format!("overwrite {}", p.display()))?;
                Ok(())
            })?;

            // unlink
            let unlink_avg = avg_us(OPS_PER_STEP, |i| {
                let id = unlink_ids[i];
                let p = file_path(root, id);
                std::fs::remove_file(&p).with_context(|| format!("unlink {}", p.display()))?;
                Ok(())
            })?;
            file_ids.retain(|id| !unlink_ids.contains(id));

            // create (replace the unlinked files — keeps directory size constant)
            let mut created = Vec::with_capacity(OPS_PER_STEP);
            let create_avg = avg_us(OPS_PER_STEP, |_| {
                let id = next_id;
                next_id += 1;
                let p = file_path(root, id);
                std::fs::write(&p, seed_bytes(id))
                    .with_context(|| format!("create {}", p.display()))?;
                created.push(id);
                Ok(())
            })?;
            file_ids.extend(created);

            // checkpoint
            let checkpoint_ms = match emit_checkpoint_request(step)? {
                Some(ms) => ms,
                None => {
                    eprintln!("  step {step}/{checkpoint_steps}: backend stopped");
                    break;
                }
            };

            if step % 10 == 0 || step == 1 {
                eprintln!(
                    "  step {step}/{checkpoint_steps}: \
                     files={} chkpt={checkpoint_ms}ms \
                     stat={stat_avg:.0}µs read={read_avg:.0}µs \
                     create={create_avg:.0}µs overwrite={overwrite_avg:.0}µs \
                     unlink={unlink_avg:.0}µs",
                    file_ids.len(),
                );
            }

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

        eprintln!(
            "  done: {} checkpoints recorded, {} files",
            points.len(),
            file_ids.len()
        );

        let series = CheckpointLatencySeries { points };
        let json = serde_json::to_string(&series).context("serializing checkpoint series")?;
        let stdout = std::io::stdout();
        let mut out = stdout.lock();
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

fn emit_checkpoint_request(step: usize) -> Result<Option<u64>> {
    // Save cwd and move to / so the backend can unmount+remount freely.
    let saved_cwd = std::env::current_dir().context("getting cwd before checkpoint")?;
    std::env::set_current_dir("/").context("chdir to / before checkpoint")?;

    println!("{}", crate::backend::CHECKPOINT_MARKER);
    println!("{{\"step\":{step}}}");
    std::io::stdout()
        .flush()
        .context("flushing checkpoint request")?;

    let stdin = std::io::stdin();
    let mut lock = stdin.lock();
    let mut line = String::new();

    line.clear();
    let n = lock
        .read_line(&mut line)
        .context("reading checkpoint response marker")?;
    if n == 0 || line.trim() != crate::backend::RESULTS_MARKER {
        bail!(
            "expected {} after checkpoint request",
            crate::backend::RESULTS_MARKER
        );
    }

    line.clear();
    let n = lock
        .read_line(&mut line)
        .context("reading checkpoint response json")?;
    if n == 0 {
        bail!("missing checkpoint response JSON");
    }

    #[derive(serde::Deserialize)]
    struct Resp {
        checkpoint_ms: u64,
        stop: bool,
        #[serde(default)]
        next_dest: Option<String>,
    }
    let resp: Resp = serde_json::from_str(line.trim())
        .with_context(|| format!("parsing checkpoint response: {}", line.trim()))?;
    if resp.stop {
        return Ok(None);
    }
    // chdir to the backend-provided path, or back to the saved cwd.
    let target = resp
        .next_dest
        .as_deref()
        .map(Path::new)
        .unwrap_or(&saved_cwd);
    std::env::set_current_dir(target)
        .with_context(|| format!("chdir to {} after checkpoint", target.display()))?;
    Ok(Some(resp.checkpoint_ms))
}

fn normalize_dest(path: &Path) -> PathBuf {
    if path.file_name().is_some_and(|n| n == ".") {
        path.parent().unwrap_or(path).to_path_buf()
    } else {
        path.to_path_buf()
    }
}

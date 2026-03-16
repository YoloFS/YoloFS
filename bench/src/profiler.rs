// agfs-bench profiler: bpftrace op latency + perf flamegraph.
//
// Profiler::start() spawns bpftrace (with a generated kfunc script for the
// interesting agfs_* functions) and perf record. The bpftrace script uses
// bpftrace's built-in hist() aggregation: entry stores nsecs in @start[tid],
// return accumulates the elapsed time into @latency[func]. On SIGINT bpftrace
// flushes all maps, producing per-op latency histograms in bpftrace.txt.
//
// Profiler::stop() signals both tools, parses the histogram output to compute
// per-op stats (median, p99, total), writes summary.txt / bpftrace.txt /
// stacks.txt / flamegraph.svg into the given output directory.

use anyhow::{Context, Result, bail};
use std::fs;
use std::io::{BufRead as _, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

// ── Public types ──────────────────────────────────────────────────────────────

pub struct Profiler {
    bpftrace: Option<Child>,
    perf: Child,
    bpf_output: Option<PathBuf>,
    out_dir: PathBuf,
}

pub struct OpStats {
    pub name: String,
    pub count: usize,
    pub median_us: u64,
    pub p99_us: u64,
    pub total_ms: f64,
}

// ── Profiler ──────────────────────────────────────────────────────────────────

impl Profiler {
    /// Spawn perf record and optionally bpftrace. When bpftrace is enabled,
    /// blocks until all probes are attached before returning.
    pub fn start(out_dir: &Path, run_bpftrace: bool) -> Result<Self> {
        fs::create_dir_all(out_dir).context("creating profile output dir")?;

        // Spawn perf first so it is already recording by the time bpftrace
        // finishes attaching probes (which can take several seconds). This
        // prevents a race where the workload finishes before perf initialises.
        let perf_data = out_dir.join("perf.data");
        let perf = Command::new("sudo")
            .args([
                "perf",
                "record",
                "-g",
                "-F",
                "99",
                "-p",
                &std::process::id().to_string(),
                "-o",
            ])
            .arg(&perf_data)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .context("spawning perf record (is perf installed?)")?;

        let (bpftrace, bpf_output) = if run_bpftrace {
            let funcs = discover_agfs_funcs().context("discovering agfs kfuncs")?;
            if funcs.is_empty() {
                bail!("no agfs_* kfuncs found — is the kernel module loaded?");
            }

            let script = generate_script(&funcs);
            let script_path = out_dir.join("probe.bt");
            fs::write(&script_path, &script).context("writing probe.bt")?;

            let bpf_output = out_dir.join("bpftrace.txt");

            // bpftrace routes "Attaching N probes..." to stdout (not stderr) when
            // both are piped. We read stdout in a thread: the BEGIN block prints
            // "READY" after all probes are attached; we signal on that, then
            // continue collecting the histogram dump into bpftrace.txt.
            let mut bpftrace = Command::new("sudo")
                .args(["bpftrace"])
                .arg(&script_path)
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
                .context("spawning bpftrace (is it installed?)")?;

            let stdout = bpftrace.stdout.take().unwrap();
            let (tx, rx) = std::sync::mpsc::channel::<bool>();
            let bpf_output_thread = bpf_output.clone();
            std::thread::spawn(move || {
                let mut out = fs::File::create(&bpf_output_thread).expect("creating bpftrace.txt");
                let reader = std::io::BufReader::new(stdout);
                let mut found = false;
                for line in reader.lines() {
                    let Ok(line) = line else { break };
                    if !found && line.trim() == "READY" {
                        found = true;
                        let _ = tx.send(true);
                        continue;
                    }
                    if line.starts_with("Attaching") {
                        continue;
                    }
                    let _ = writeln!(out, "{line}");
                }
                if !found {
                    let _ = tx.send(false);
                }
            });

            match rx.recv_timeout(Duration::from_secs(300)) {
                Ok(true) => {}
                Ok(false) => bail!("bpftrace exited before all probes were attached"),
                Err(_) => bail!("timed out waiting for bpftrace to attach probes"),
            }

            (Some(bpftrace), Some(bpf_output))
        } else {
            (None, None)
        };

        Ok(Self {
            bpftrace,
            perf,
            bpf_output,
            out_dir: out_dir.to_path_buf(),
        })
    }

    /// Stop both tools, compute stats, write all artifacts, print summary.
    pub fn stop(mut self, wall_ms: u64) -> Result<()> {
        if let Some(ref bpftrace) = self.bpftrace {
            // SIGINT triggers bpftrace to flush all maps before exiting.
            let _ = nix::sys::signal::kill(
                nix::unistd::Pid::from_raw(bpftrace.id() as i32),
                nix::sys::signal::Signal::SIGINT,
            );
        }
        let _ = nix::sys::signal::kill(
            nix::unistd::Pid::from_raw(self.perf.id() as i32),
            nix::sys::signal::Signal::SIGINT,
        );
        if let Some(mut bpftrace) = self.bpftrace.take() {
            bpftrace.wait().context("waiting for bpftrace")?;
        }
        self.perf.wait().context("waiting for perf")?;

        let ops = if let Some(ref bpf_output) = self.bpf_output {
            let raw = fs::read_to_string(bpf_output).context("reading bpftrace output")?;
            compute_stats(parse_histograms(&raw))
        } else {
            vec![]
        };

        write_summary(&self.out_dir, &ops, wall_ms)?;
        generate_flamegraph(&self.out_dir)?;

        Ok(())
    }
}

impl Drop for Profiler {
    fn drop(&mut self) {
        // Kill both children if stop() was never called (e.g. on early error).
        if let Some(ref bpftrace) = self.bpftrace {
            let _ = nix::sys::signal::kill(
                nix::unistd::Pid::from_raw(bpftrace.id() as i32),
                nix::sys::signal::Signal::SIGINT,
            );
        }
        let _ = nix::sys::signal::kill(
            nix::unistd::Pid::from_raw(self.perf.id() as i32),
            nix::sys::signal::Signal::SIGINT,
        );
        if let Some(ref mut bpftrace) = self.bpftrace {
            let _ = bpftrace.wait();
        }
        let _ = self.perf.wait();
    }
}

// ── Discovery and script generation ──────────────────────────────────────────

/// Hot-path functions worth tracing. We take the intersection with whatever
/// the loaded kmod actually exposes, so this list can be a superset.
const INTERESTING: &[&str] = &[
    "agfs_lookup",
    "agfs_d_revalidate",
    "agfs_permission",
    "agfs_resolve_perm",
    "agfs_open",
    "agfs_read_iter",
    "agfs_write_iter",
    "agfs_create",
    "agfs_create_staged",
    "agfs_cow_if_needed",
    "agfs_do_cow",
    "agfs_staging_alloc",
    "agfs_readdir",
    "agfs_journal_append_a",
    "agfs_journal_append_d",
    "agfs_journal_append_r",
    "agfs_release",
    "agfs_find_dirent",
];

fn discover_agfs_funcs() -> Result<Vec<String>> {
    let out = Command::new("sudo")
        .args(["bpftrace", "-l", "kfunc:agfs_*"])
        .output()
        .context("running bpftrace -l")?;
    if !out.status.success() {
        bail!(
            "bpftrace -l failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    // Lines are "kfunc:agfs:agfs_foo" — strip the "kfunc:agfs:" prefix.
    let available: std::collections::HashSet<String> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| l.strip_prefix("kfunc:agfs:"))
        .map(|s| s.to_string())
        .collect();
    Ok(INTERESTING
        .iter()
        .filter(|f| available.contains(**f))
        .map(|s| s.to_string())
        .collect())
}

fn generate_script(funcs: &[String]) -> String {
    // BEGIN fires after all probes are attached — we wait for this marker
    // before starting the workload, ensuring no samples are missed.
    //
    // Each function gets its own @start_<func>[tid] map so that nested calls
    // between interesting functions don't clobber each other's entry timestamp.
    let mut s = String::from("BEGIN { printf(\"READY\\n\"); }\n");
    for func in funcs {
        // Strip "agfs_" prefix for the map name to stay under BPF name limits.
        let short = func.strip_prefix("agfs_").unwrap_or(func);
        s.push_str(&format!(
            "kfunc:agfs:{func} {{ @s_{short}[tid] = nsecs; }}\n\
             kretfunc:agfs:{func} {{ if (@s_{short}[tid] != 0) {{\
               @{func} = hist((nsecs - @s_{short}[tid]) / 1000);\
               delete(@s_{short}[tid]); }} }}\n"
        ));
    }
    s
}

// ── Parsing and statistics ────────────────────────────────────────────────────

// Parsed representation of one bpftrace log2 histogram bucket.
struct Bucket {
    lo: u64, // inclusive lower bound (µs)
    hi: u64, // exclusive upper bound (µs); lo+1 for point values
    count: u64,
}

// One histogram (one agfs function) parsed from bpftrace output.
struct Histogram {
    name: String,
    buckets: Vec<Bucket>,
}

/// Parse a bpftrace integer that may carry a K/M/G suffix (powers of 1024).
fn parse_bpf_int(s: &str) -> u64 {
    if let Some(n) = s.strip_suffix('K') {
        n.trim().parse::<u64>().unwrap_or(0) * 1024
    } else if let Some(n) = s.strip_suffix('M') {
        n.trim().parse::<u64>().unwrap_or(0) * 1024 * 1024
    } else if let Some(n) = s.strip_suffix('G') {
        n.trim().parse::<u64>().unwrap_or(0) * 1024 * 1024 * 1024
    } else {
        s.parse().unwrap_or(0)
    }
}

/// Parse bpftrace hist() output.
///
/// Expected format (emitted on SIGINT map flush):
/// ```
/// @latency["agfs_permission"]:
/// [0]                  892 |@@...    |
/// [1]                 3421 |@@...    |
/// [2, 4)               234 |@...     |
/// [4, 8)               156 |         |
/// ...
///
/// @latency["agfs_lookup"]:
/// ...
/// ```
fn parse_histograms(raw: &str) -> Vec<Histogram> {
    let mut hists: Vec<Histogram> = Vec::new();
    let mut current: Option<Histogram> = None;

    for line in raw.lines() {
        // New histogram header: @agfs_foo:
        if let Some(rest) = line.strip_prefix('@')
            && let Some(name) = rest.trim_end().strip_suffix(':')
                && !name.is_empty() && !name.contains('[') {
                    if let Some(current) = current.take() {
                        hists.push(current);
                    }
                    current = Some(Histogram {
                        name: name.to_string(),
                        buckets: Vec::new(),
                    });
                    continue;
                }

        let Some(ref mut hist) = current else {
            continue;
        };

        // Bucket line: leading whitespace, then '[' ...
        let trimmed = line.trim();
        if !trimmed.starts_with('[') {
            continue;
        }

        // Parse "[lo, hi)  count |...|" or "[lo]  count |...|"
        // or "(..., hi)  count |...|" (negative bucket — skip)
        if trimmed.starts_with('(') {
            continue;
        }

        // Closing delimiter is ']' for point values ([0], [1]) or ')' for ranges ([2, 4)).
        let close = match trimmed.find([']', ')']) {
            Some(i) => i,
            None => continue,
        };
        let range = &trimmed[1..close]; // "lo" or "lo, hi"
        let after = trimmed[close + 1..].trim();
        let count: u64 = match after.split_whitespace().next().and_then(|s| s.parse().ok()) {
            Some(v) => v,
            None => continue,
        };

        let (lo, hi) = if let Some((lo_s, hi_s)) = range.split_once(',') {
            let lo = parse_bpf_int(lo_s.trim());
            let hi = parse_bpf_int(hi_s.trim().trim_end_matches(')').trim());
            (lo, hi)
        } else {
            let v = parse_bpf_int(range.trim());
            (v, v + 1)
        };

        hist.buckets.push(Bucket { lo, hi, count });
    }
    if let Some(current) = current.take() {
        hists.push(current);
    }
    hists
}

fn compute_stats(hists: Vec<Histogram>) -> Vec<OpStats> {
    let mut stats: Vec<OpStats> = hists
        .into_iter()
        .filter_map(|h| {
            let total_count: u64 = h.buckets.iter().map(|b| b.count).sum();
            if total_count == 0 {
                return None;
            }
            // Weighted sum for total_ms: use bucket midpoint as representative value.
            let total_us: f64 = h
                .buckets
                .iter()
                .map(|b| (b.lo + b.hi - 1) as f64 / 2.0 * b.count as f64)
                .sum();
            let total_ms = total_us / 1000.0;

            let median_us = percentile(&h.buckets, total_count, 50);
            let p99_us = percentile(&h.buckets, total_count, 99);

            Some(OpStats {
                name: h.name,
                count: total_count as usize,
                median_us,
                p99_us,
                total_ms,
            })
        })
        .collect();
    // Rank by total time descending.
    stats.sort_by(|a, b| b.total_ms.partial_cmp(&a.total_ms).unwrap());
    stats
}

/// Return the lower bound of the bucket containing the Nth percentile.
fn percentile(buckets: &[Bucket], total: u64, pct: u64) -> u64 {
    let target = (total * pct).div_ceil(100); // ceil
    let mut acc = 0u64;
    for b in buckets {
        acc += b.count;
        if acc >= target {
            return b.lo;
        }
    }
    buckets.last().map(|b| b.lo).unwrap_or(0)
}

// ── Output ────────────────────────────────────────────────────────────────────

fn write_summary(out_dir: &Path, ops: &[OpStats], wall_ms: u64) -> Result<()> {
    // out_dir is  …/profiling/<workload>/<scenario>
    let scenario = out_dir.file_name().and_then(|n| n.to_str()).unwrap_or("?");
    let workload = out_dir
        .parent()
        .and_then(|p| p.file_name())
        .and_then(|n| n.to_str())
        .unwrap_or("?");

    let mut buf = String::new();
    buf.push_str(&format!(
        "Profile: {workload} / {scenario}  (wall: {wall_ms} ms)\n\n"
    ));

    if ops.is_empty() {
        buf.push_str("  (no bpftrace data)\n");
    } else {
        buf.push_str(&format!(
            "  {:<32}  {:>8}  {:>8}  {:>6}  {:>10}\n",
            "op", "calls", "median µs", "p99 µs", "total ms"
        ));
        buf.push_str(&format!("  {}\n", "-".repeat(76)));
        for op in ops {
            let short = op.name.strip_prefix("agfs_").unwrap_or(&op.name);
            buf.push_str(&format!(
                "  {:<32}  {:>8}  {:>9}  {:>6}  {:>10.1}\n",
                short, op.count, op.median_us, op.p99_us, op.total_ms
            ));
        }
    }

    print!("{buf}");

    let summary_path = out_dir.join("summary.txt");
    fs::write(&summary_path, &buf).context("writing summary.txt")?;
    eprintln!("Profile artifacts written to {}", out_dir.display());

    Ok(())
}

fn generate_flamegraph(out_dir: &Path) -> Result<()> {
    let perf_data = out_dir.join("perf.data");
    if !perf_data.exists() {
        return Ok(());
    }

    let script = Command::new("sudo")
        .args(["perf", "script", "-i"])
        .arg(&perf_data)
        .output()
        .context("running perf script")?;

    // Collapse stacks.
    let mut collapsed: Vec<u8> = Vec::new();
    {
        use inferno::collapse::Collapse;
        use inferno::collapse::perf::{Folder, Options};
        let mut folder = Folder::from(Options::default());
        folder
            .collapse(script.stdout.as_slice(), &mut collapsed)
            .context("collapsing perf stacks")?;
    }
    fs::write(out_dir.join("stacks.txt"), &collapsed).context("writing stacks.txt")?;

    // Generate SVG.
    let mut svg =
        fs::File::create(out_dir.join("flamegraph.svg")).context("creating flamegraph.svg")?;
    {
        use inferno::flamegraph::{self, Options};
        flamegraph::from_reader(&mut Options::default(), collapsed.as_slice(), &mut svg)
            .context("generating flamegraph.svg")?;
    }

    Ok(())
}

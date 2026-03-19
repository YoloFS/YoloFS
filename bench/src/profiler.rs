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
            .args(["perf", "record", "-a", "-g", "-F", "997", "-o"])
            .arg(&perf_data)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .context("spawning perf record (is perf installed?)")?;

        // Give perf time to initialise before the workload starts.
        std::thread::sleep(Duration::from_millis(300));

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
    pub fn stop(mut self, wall_ms: u64, iters: u32) -> Result<()> {
        if let Some(ref bpftrace) = self.bpftrace {
            // SIGINT triggers bpftrace to flush all maps before exiting.
            let _ = nix::sys::signal::kill(
                nix::unistd::Pid::from_raw(bpftrace.id() as i32),
                nix::sys::signal::Signal::SIGINT,
            );
        }
        // sudo perf runs as a child; kill the entire process group so
        // SIGINT reaches perf underneath sudo.
        let _ = Command::new("sudo")
            .args(["kill", "-INT"])
            .arg(self.perf.id().to_string())
            .status();
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
        generate_breakdown(&self.out_dir, wall_ms, iters)?;

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
    "agfs_journal_add",
    "agfs_journal_modify",
    "agfs_journal_delete",
    "agfs_journal_redirect",
    "agfs_journal_checkpoint",
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
            && !name.is_empty()
            && !name.contains('[')
        {
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
    let target = (total * pct).div_ceil(100);
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

    // perf.data is owned by root (system-wide recording with sudo).
    // Filter to just our process's comm name to exclude noise.
    let script = Command::new("sudo")
        .args(["perf", "script", "--comms=agfs-bench", "-i"])
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

/// Generate a function-level breakdown from collapsed stacks, sorted by
/// sample count. Each line shows the percentage of total samples and the
/// estimated wall-time contribution based on the profiled duration.
/// Return patterns that identify the workload's run function in collapsed
/// stacks. We match either the shared `run_meta_*` helper or the trait
/// impl `Workload>::run`.
fn workload_run_patterns(out_dir: &Path) -> Vec<String> {
    let workload_name = out_dir
        .parent()
        .and_then(|p| p.file_name())
        .and_then(|n| n.to_str())
        .unwrap_or("");
    let base = crate::workloads::meta_shared::source_group_name(workload_name)
        .unwrap_or(workload_name);
    let snake = base.replace('-', "_");
    vec![
        // Shared helper: agfs_bench::workloads::meta_shared::run_meta_open_warm
        format!("run_{snake}"),
        // Trait impl: <...::MetaCreate as ...::Workload>::run
        "::Workload>::run".to_string(),
    ]
}

fn stack_matches_run(stack: &str, patterns: &[String]) -> bool {
    patterns.iter().any(|p| stack.contains(p))
}

// ── Call tree ─────────────────────────────────────────────────────────────────

/// A node in the call tree built from collapsed stacks.
#[derive(serde::Serialize, serde::Deserialize)]
struct CallNode {
    name: String,
    /// Inclusive sample count (this function + all descendants).
    inclusive: u64,
    /// Self sample count (stacks that terminate at this function).
    self_count: u64,
    children: Vec<CallNode>,
}

/// Serialisable breakdown metadata saved alongside the call tree.
#[derive(serde::Serialize, serde::Deserialize)]
struct BreakdownMeta {
    workload: String,
    backend: String,
    wall_ms: u64,
    iters: u32,
    per_op_us: Option<f64>,
    timed_samples: u64,
    tree: CallNode,
}

impl CallNode {
    fn new(name: &str) -> Self {
        Self {
            name: name.to_string(),
            inclusive: 0,
            self_count: 0,
            children: Vec::new(),
        }
    }

    fn child_mut(&mut self, name: &str) -> &mut CallNode {
        if let Some(pos) = self.children.iter().position(|c| c.name == name) {
            &mut self.children[pos]
        } else {
            self.children.push(CallNode::new(name));
            self.children.last_mut().unwrap()
        }
    }

    /// Sort children by inclusive count (descending), recursively.
    fn sort(&mut self) {
        self.children.sort_by(|a, b| b.inclusive.cmp(&a.inclusive));
        for child in &mut self.children {
            child.sort();
        }
    }

    /// Render the tree as indented text. `scale` converts sample counts to µs.
    fn render(&self, buf: &mut String, scale: f64, prefix: &str, is_last: bool, is_root: bool) {
        let incl_us = self.inclusive as f64 * scale;
        let self_us = self.self_count as f64 * scale;

        if !is_root {
            let connector = if is_last { "└─ " } else { "├─ " };
            let self_str = if self_us > 0.005 {
                format!("{:5.2}", self_us)
            } else {
                "    —".to_string()
            };
            buf.push_str(&format!(
                "  {:6.2}  {}  {}{}{}\n",
                incl_us, self_str, prefix, connector, self.name
            ));
        }

        let child_prefix = if is_root {
            String::new()
        } else if is_last {
            format!("{prefix}   ")
        } else {
            format!("{prefix}│  ")
        };

        // Only show children that are at least 1% of parent's inclusive.
        let threshold = (self.inclusive as f64 * 0.01) as u64;
        let visible: Vec<&CallNode> = self
            .children
            .iter()
            .filter(|c| c.inclusive >= threshold)
            .collect();

        for (i, child) in visible.iter().enumerate() {
            let last = i == visible.len() - 1;
            child.render(buf, scale, &child_prefix, last, false);
        }
    }
}

/// Build a call tree from collapsed stacks, filtering and trimming as needed.
fn build_call_tree<'a>(
    stacks: impl Iterator<Item = (&'a str, u64)>,
    skip: &dyn Fn(&str) -> bool,
) -> CallNode {
    let mut root = CallNode::new("(root)");

    for (stack, count) in stacks {
        let funcs: Vec<&str> = stack.split(';').filter(|f| !skip(f)).collect();
        if funcs.is_empty() {
            continue;
        }
        root.inclusive += count;

        let mut node = &mut root;
        for (i, func) in funcs.iter().enumerate() {
            node = node.child_mut(func);
            node.inclusive += count;
            if i == funcs.len() - 1 {
                node.self_count += count;
            }
        }
    }

    root.sort();
    root
}

fn generate_breakdown(out_dir: &Path, wall_ms: u64, iters: u32) -> Result<()> {
    let stacks_path = out_dir.join("stacks.txt");
    if !stacks_path.exists() {
        return Ok(());
    }
    let raw = fs::read_to_string(&stacks_path).context("reading stacks.txt")?;

    let backend_name = out_dir
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("?");
    let workload_name = out_dir
        .parent()
        .and_then(|p| p.file_name())
        .and_then(|n| n.to_str())
        .unwrap_or("?");

    let per_op_us = lookup_per_op_latency(out_dir, workload_name, backend_name);
    let run_patterns = workload_run_patterns(out_dir);

    let skip = |func: &str| -> bool {
        func == "agfs-bench"
            || func == "_start"
            || func == "main"
            || func == "[libc.so.6]"
            || func.starts_with("std::")
            || func.starts_with("core::")
            || func.starts_with("_Z")
    };

    // Parse stacks, filter to run function, exclude warm-up.
    let mut timed_total: u64 = 0;
    let mut all_total: u64 = 0;
    let mut warmup_total: u64 = 0;
    let mut run_total: u64 = 0;

    struct ParsedStack {
        stack: String,
        count: u64,
        is_run: bool,
        is_warmup: bool,
    }

    let mut parsed: Vec<ParsedStack> = Vec::new();
    for line in raw.lines() {
        let Some((stack, count_str)) = line.rsplit_once(' ') else {
            continue;
        };
        let Ok(count) = count_str.parse::<u64>() else {
            continue;
        };
        if !stack.contains("agfs-bench") {
            continue;
        }
        all_total += count;
        let is_run = stack_matches_run(stack, &run_patterns);
        let is_warmup =
            is_run && (stack.contains("warm_metadata") || stack.contains("warm_readdir"));
        if is_run {
            run_total += count;
            if is_warmup {
                warmup_total += count;
            } else {
                timed_total += count;
            }
        }
        parsed.push(ParsedStack {
            stack: stack.to_string(),
            count,
            is_run,
            is_warmup,
        });
    }

    if timed_total == 0 {
        return Ok(());
    }

    // Build call tree from timed stacks only.
    let tree = build_call_tree(
        parsed
            .iter()
            .filter(|p| p.is_run && !p.is_warmup)
            .map(|p| (p.stack.as_str(), p.count)),
        &skip,
    );

    let scale = if let Some(per_op) = per_op_us {
        per_op / timed_total as f64
    } else {
        wall_ms as f64 / all_total as f64
    };

    let mut buf = String::new();
    buf.push_str(&format!(
        "Function breakdown: {workload_name} / {backend_name}\n\
         wall: {wall_ms} ms, {iters} iterations\n"
    ));
    if let Some(us) = per_op_us {
        buf.push_str(&format!("per-op latency (from results.json): {us:.2} µs\n"));
    }

    let run_pct = run_total as f64 * 100.0 / all_total as f64;
    let warmup_pct = warmup_total as f64 * 100.0 / run_total as f64;
    let timed_pct = timed_total as f64 * 100.0 / run_total as f64;
    buf.push_str(&format!(
        "\nrun function: {run_pct:.0}% of profiled time\n\
         timed operation: {timed_pct:.0}% of run | warm-up: {warmup_pct:.0}% of run\n\n"
    ));

    let unit = if per_op_us.is_some() { "µs/op" } else { "~ms" };
    buf.push_str(&format!("  {:>6}  {:>5}  call tree\n", "incl", "self"));
    buf.push_str(&format!(
        "  {:>6}  {:>5}\n",
        unit, unit
    ));
    buf.push_str(&format!("  {}\n", "-".repeat(60)));

    // Render from the root's children (skip the synthetic root node).
    for (i, child) in tree.children.iter().enumerate() {
        let last = i == tree.children.len() - 1;
        child.render(&mut buf, scale, "", last, false);
    }

    let path = out_dir.join("breakdown.txt");
    fs::write(&path, &buf).context("writing breakdown.txt")?;
    print!("{buf}");

    // Save structured breakdown as JSON for the comparison tool.
    let meta = BreakdownMeta {
        workload: workload_name.to_string(),
        backend: backend_name.to_string(),
        wall_ms,
        iters,
        per_op_us,
        timed_samples: timed_total,
        tree,
    };
    let json_path = out_dir.join("breakdown.json");
    let json = serde_json::to_string_pretty(&meta).context("serialising breakdown")?;
    fs::write(&json_path, json).context("writing breakdown.json")?;

    Ok(())
}

/// Generate a side-by-side hierarchical comparison from breakdown.json files.
pub fn generate_comparison(prof_workload_dir: &Path) -> Result<()> {
    // Load all breakdown.json files.
    let mut metas: Vec<BreakdownMeta> = Vec::new();
    for entry in fs::read_dir(prof_workload_dir).context("reading profiling dir")? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let json_path = entry.path().join("breakdown.json");
        if let Ok(raw) = fs::read_to_string(&json_path) {
            if let Ok(meta) = serde_json::from_str::<BreakdownMeta>(&raw) {
                metas.push(meta);
            }
        }
    }
    metas.sort_by(|a, b| a.backend.cmp(&b.backend));
    if metas.len() < 2 {
        return Ok(());
    }

    let workload_name = &metas[0].workload;

    // Build a merged tree: walk all trees in parallel, unioning the children.
    // Each node stores µs/op per backend.
    struct MergedNode {
        name: String,
        /// µs/op inclusive, one per backend (0.0 if absent).
        inclusive: Vec<f64>,
        /// µs/op self, one per backend.
        self_us: Vec<f64>,
        children: Vec<MergedNode>,
    }

    fn merge_trees(trees: &[&CallNode], scales: &[f64], n: usize) -> MergedNode {
        let mut merged = MergedNode {
            name: trees.first().map(|t| t.name.clone()).unwrap_or_default(),
            inclusive: (0..n)
                .map(|i| {
                    trees
                        .get(i)
                        .map(|t| t.inclusive as f64 * scales[i])
                        .unwrap_or(0.0)
                })
                .collect(),
            self_us: (0..n)
                .map(|i| {
                    trees
                        .get(i)
                        .map(|t| t.self_count as f64 * scales[i])
                        .unwrap_or(0.0)
                })
                .collect(),
            children: Vec::new(),
        };

        // Union all child names across all trees.
        let mut child_names: Vec<String> = Vec::new();
        for tree in trees {
            for child in &tree.children {
                if !child_names.contains(&child.name) {
                    child_names.push(child.name.clone());
                }
            }
        }

        for child_name in &child_names {
            let child_trees: Vec<&CallNode> = trees
                .iter()
                .map(|t| {
                    t.children
                        .iter()
                        .find(|c| c.name == *child_name)
                        .unwrap_or(&EMPTY_NODE)
                })
                .collect();
            let child = merge_trees(&child_trees, scales, n);
            // Skip nodes where all backends have < 1% of max inclusive.
            let max_incl = child
                .inclusive
                .iter()
                .fold(0.0f64, |a, &b| a.max(b));
            let parent_max = merged
                .inclusive
                .iter()
                .fold(0.0f64, |a, &b| a.max(b));
            if max_incl >= parent_max * 0.01 {
                merged.children.push(child);
            }
        }

        // Sort children by max inclusive across backends.
        merged.children.sort_by(|a, b| {
            let max_a = a.inclusive.iter().fold(0.0f64, |x, &y| x.max(y));
            let max_b = b.inclusive.iter().fold(0.0f64, |x, &y| x.max(y));
            max_b.partial_cmp(&max_a).unwrap_or(std::cmp::Ordering::Equal)
        });

        merged
    }

    static EMPTY_NODE: CallNode = CallNode {
        name: String::new(),
        inclusive: 0,
        self_count: 0,
        children: Vec::new(),
    };

    let n = metas.len();
    let scales: Vec<f64> = metas
        .iter()
        .map(|m| {
            m.per_op_us.unwrap_or(m.wall_ms as f64)
                / m.timed_samples.max(1) as f64
        })
        .collect();

    // Get root children from each tree (skip the synthetic root).
    let roots: Vec<&CallNode> = metas.iter().map(|m| &m.tree).collect();
    let merged = merge_trees(&roots, &scales, n);

    // Render.
    let mut buf = String::new();
    buf.push_str(&format!("Comparison: {workload_name}\n"));
    buf.push_str(&format!("{}\n\n", "=".repeat(70)));
    for m in &metas {
        buf.push_str(&format!(
            "  {}: {:.2} µs/op\n",
            m.backend,
            m.per_op_us.unwrap_or(0.0)
        ));
    }
    buf.push('\n');

    // Column header.
    let backend_labels: Vec<&str> = metas.iter().map(|m| m.backend.as_str()).collect();
    buf.push_str(&format!("  {:<35}", ""));
    for label in &backend_labels {
        let short = if label.len() > 10 { &label[..10] } else { label };
        buf.push_str(&format!(" {:>10}", short));
    }
    if n == 2 {
        buf.push_str(&format!(" {:>8}", "delta"));
    }
    buf.push('\n');
    buf.push_str(&format!("  {}\n", "-".repeat(35 + n * 11 + if n == 2 { 9 } else { 0 })));

    fn render_merged(
        node: &MergedNode,
        buf: &mut String,
        prefix: &str,
        is_last: bool,
        n: usize,
    ) {
        let connector = if is_last { "└─ " } else { "├─ " };
        buf.push_str(&format!("  {prefix}{connector}{:<width$}", node.name, width = 33_usize.saturating_sub(prefix.len() + 3)));
        for &us in &node.inclusive {
            if us > 0.005 {
                buf.push_str(&format!(" {:>9.2}", us));
            } else {
                buf.push_str(&format!(" {:>10}", "—"));
            }
        }
        if n == 2 {
            let delta = node.inclusive[0] - node.inclusive[1];
            if delta.abs() > 0.01 {
                buf.push_str(&format!(" {:>+7.2}", delta));
            }
        }
        buf.push('\n');

        let child_prefix = if is_last {
            format!("{prefix}   ")
        } else {
            format!("{prefix}│  ")
        };

        for (i, child) in node.children.iter().enumerate() {
            let last = i == node.children.len() - 1;
            render_merged(child, buf, &child_prefix, last, n);
        }
    }

    for (i, child) in merged.children.iter().enumerate() {
        let last = i == merged.children.len() - 1;
        render_merged(child, &mut buf, "", last, n);
    }

    let path = prof_workload_dir.join("comparison.txt");
    fs::write(&path, &buf).context("writing comparison.txt")?;
    eprintln!("Comparison written to {}", path.display());
    print!("{buf}");

    Ok(())
}

/// Look up the per-operation latency (µs) for a workload+backend from
/// the most recent results.json. Returns None if not found.
fn lookup_per_op_latency(out_dir: &Path, workload_name: &str, backend_name: &str) -> Option<f64> {
    // Walk up from profiling/<workload>/<backend> to find results.json.
    let results_dir = out_dir.parent()?.parent()?.parent()?;
    let json_path = results_dir.join("results.json");
    let raw = fs::read_to_string(&json_path).ok()?;
    let results: serde_json::Value = serde_json::from_str(&raw).ok()?;
    for wl in results["workloads"].as_array()? {
        if wl["workload"].as_str()? != workload_name {
            continue;
        }
        for b in wl["backends"].as_array()? {
            if b["backend"].as_str()? != backend_name {
                continue;
            }
            let iops = b["mean_iops"].as_f64()?;
            if iops > 0.0 {
                return Some(1_000_000.0 / iops);
            }
        }
    }
    None
}

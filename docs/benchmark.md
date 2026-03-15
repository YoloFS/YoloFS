# agfs — Evaluation & Benchmarking

This document describes the evaluation strategy for agfs: what to measure,
why, and how the benchmark suite is structured.

---

## 1. Goals

agfs adds overhead over a native filesystem in two areas:

1. **VFS interposition** — every syscall passes through agfs's stackable ops
   before reaching the lower filesystem.
2. **Permission gating** — each file access may be resolved from a per-inode
   cache, matched against a rule, or round-tripped through a userspace daemon.

Writes additionally incur staging costs: data is written to a blob in the
staging directory rather than directly to the base filesystem, and eventually
flushed to the base on `commit`.

The benchmark suite produces comprehensive, reproducible results demonstrating
agfs overhead across realistic workloads and permission configurations.

---

## 2. Workloads

The suite defines multiple workloads to avoid overfitting to a single access
pattern. Each workload is a self-contained Rust function that performs a
specific operation on the mounted filesystem.

### Write files (`write-files`)

Creates 1,000 small files (4 KiB each) in a temporary directory. Exercises the
file-create and sequential-write paths without any network dependency; no
external fixture is required. Useful for rapid iteration and as a quick sanity
check before running heavier workloads.

### Worktree (`worktree`)

Runs `git worktree add --detach` from a local Linux kernel clone
(`~/.cache/agfs-bench/linux`). Exercises the read-heavy path: the workload
reads thousands of objects from the base repository and writes a new working
tree into the agfs mount. The fixture (initial clone) is constructed once and
reused; subsequent runs use `git worktree prune` to clean up stale entries
before each `worktree add`.

---

## 3. Permission Scenarios

Each workload is run under each of the following permission configurations.
The goal is to isolate the cost of each level of gating.

| Scenario | Configuration | What it measures |
|---|---|---|
| `native` | No agfs, direct ext4 | True baseline |
| `rules-allow-all` | `allow-rw /` rule | VFS interposition + staging; no per-access gating |
| `rules-realistic` | workload-defined rules | Typical rule-based config; most accesses hit cache |

`rules-allow-all` is the practical floor for a useful agfs configuration: all
writes are staged, all reads pass through VFS interposition, but no gating
overhead is paid per-access. `native` is the absolute floor.

---

## 4. Timing Model

For write workloads, time is decomposed into two phases and reported
separately:

```
total = staging_time + commit_time
```

- **`staging_time`**: wall time of the workload itself while running inside
  agfs. This is what the agent experiences.
- **`commit_time`**: wall time of `agfs commit` after the workload completes.
  This is the cost of flushing staged changes to the base filesystem.

All timings are taken with `std::time::Instant` inside the bench binary.
Each (workload, scenario) pair is run `N_ITERS` times (currently 3), preceded
by one warm-up run; mean ± stddev are reported, and outliers (>2σ) are flagged.

---

## 5. Fixture vs Run

**Fixture** (setup, not timed): constructed once and reused across all
subsequent runs. If the fixture already exists it is not rebuilt.

- Each workload declares its own fixture requirements via `ensure_fixture()`,
  called once before any scenarios run for that workload.
- `worktree`: clones the Linux kernel to `~/.cache/agfs-bench/linux`.
- `write-files`: no external fixture needed.

**Warm-up**: one warm-up run is performed in `native` mode before all
scenarios for a workload begin. It populates the page cache and warms
dentry/inode caches. The warm-up result is discarded.

**Run** (timed): each scenario runs N timed iterations. Currently N=3 by
default; increase `N_ITERS` for more statistical confidence. Each iteration
creates a fresh `TempDir` and agfs mount to avoid stale dentry state.
Mean ± stddev of the N timed iterations is reported; outliers (>2σ) are
flagged.

**Teardown**: the agfs mount and session directory are removed automatically
when the `Session` is dropped at the end of each iteration.

---

## 6. Implementation

The benchmark suite is a Rust binary (`agfs-bench`) in the same Cargo
workspace as the CLI, under `bench/src/`. It shares ioctl types, mount
helpers, config parsing, and klog utilities with the CLI via the existing
library crate.

The user-facing interface is:

```
agfs-bench [--workload <name>] [--scenario <name>] [--verbose] [--timestamped-results]
agfs-bench rerender
agfs-bench list
agfs-bench profile [--workload <name>] [--scenario <name>] [--perf]
```

- With no flags: runs all workloads × all scenarios.
- `--workload` / `--scenario`: filter to a single combination.
- `--verbose`: capture detailed logs for all runs, not just failures.
- `--timestamped-results`: write results into a timestamped subdirectory
  (`bench-results/<hostname>/<timestamp>/`) instead of overwriting.
- `rerender`: regenerate the HTML report from the existing `results.json`
  without re-running any benchmarks. Useful when iterating on the
  visualisation.
- `list`: print all registered workload names.
- `profile`: run the profiling mode (see §7).

### Logging and failure handling

Each run captures basic timing and workload output. On failure, the failing
(workload, scenario) combination is automatically rerun with verbose logging
enabled. Verbose logs include:

- Workload stdout/stderr
- Kernel messages captured via `klog::snapshot` / `klog::since` (systemd
  journal, kernel transport only)
- agfs journal contents at the point of failure, parsed via
  `agfs::journal::read`

### Results

Results are written to `bench-results/<hostname>/`. By default the previous
result for that host is overwritten; pass `--timestamped-results` to retain
multiple runs from the same machine.

Each result records environment metadata so results from different machines are
not conflated:

- **Hardware**: CPU model, memory size, storage device and model
- **Filesystem**: type, total size, free space, mount options (from
  `/proc/mounts` + `statvfs`)
- **Software**: kernel version, Linux distribution

Results are written to `results.json` with the following structure:

```json
{
  "env": { "hostname": "...", "cpu": "...", "storage_device_model": "...", ... },
  "workloads": [
    {
      "workload": "worktree",
      "scenarios": [
        { "scenario": "native", "iters": [...], "mean_ms": 0, "stddev_ms": 0 },
        ...
      ]
    },
    { "workload": "write-files", "scenarios": [...] }
  ]
}
```

Running `--workload X` merges the result for workload X into the existing
`results.json`, preserving results for other workloads.

An HTML report (`report-<workload>.html`) is generated per workload using the
[`plotly`](https://crates.io/crates/plotly) crate:

- Stacked bar charts: scenario × (staging time, commit time).
- Native rendered as a bar alongside the agfs scenarios for direct visual
  comparison.
- Error bars on the top of each stack showing total stddev across iterations.

`make bench` builds and runs `agfs-bench`. It is not part of the default CI
pipeline; it is triggered manually via `workflow_dispatch` on a dedicated
GitHub Actions workflow, or run locally with `make bench` (which handles
`load-kmod` and `install` automatically).

---

## 7. Profiling

`agfs-bench profile` identifies *where* agfs overhead goes, to guide
optimization. It runs a single iteration (no warmup, no averaging) with
profiling tools active alongside the workload.

### What to measure

agfs overhead has three distinct sources, each requiring a different tool:

| Source | Cost | Tool |
|---|---|---|
| VFS interposition | Every syscall through agfs stackable ops | bpftrace kprobe latency histograms |
| Write staging | Each write redirected to a staging blob | `/proc/<pid>/io` write amplification |
| Permission gating | Per-access cache lookup + rule match | bpftrace on `agfs_permission` separately |

### Tier 1 — bpftrace op latency histograms

A bpftrace script runs as a child process for the duration of the workload,
instrumenting these agfs kfunctions:

| Function | What it covers |
|---|---|
| `agfs_lookup` | Dentry resolution (every path component) |
| `agfs_d_revalidate` | Dentry cache validation |
| `agfs_permission` | Permission check (inode cache + rule match) |
| `agfs_open` | File open, including COW trigger |
| `agfs_create` | File creation + staging entry allocation |
| `agfs_read_iter` | Read path (lower fs or staging blob) |
| `agfs_write_iter` | Write path (always to staging blob) |
| `agfs_cow_if_needed` | First-write copy-on-write trigger |
| `agfs_readdir` | Directory listing merged from base + staging |

Each function is measured with `hist((nsecs - @start[tid]) / 1000)` on
kretprobe (microsecond granularity) and a call counter.

**Startup sequencing**: bpftrace takes ~0.5–1 s to attach kprobes. The bench
spawns it, polls stderr until a line starting with `"Attaching"` appears, then
starts the workload. After the workload completes, SIGINT is sent to bpftrace
(causing it to flush and print all maps), and its stdout is collected.

No PID filter is needed: the agfs mount is unique to this bench session, so
all `agfs_*` activity during the window belongs to the workload.

For the `native` scenario, no agfs functions fire, which confirms zero
interposition overhead and provides a clean baseline for I/O amplification.

### Tier 2 — I/O amplification via `/proc/self/io`

`rchar`, `wchar`, `read_bytes`, `write_bytes` are read before and after the
workload. For write-files (1,000 × 4 KiB = 4 MB logical), the ratio
`write_bytes / logical_writes` shows the staging amplification factor (journal
appends + blob creation on top of the raw payload).

### Tier 3 — perf flamegraph (optional, `--perf`)

`perf record -g -F 99 -p <self-pid>` runs as a side-car for the duration of
the workload. The resulting `perf.data` is saved to the profile directory.
Generate the flamegraph with:

```
perf script -i perf.data | inferno-collapse-perf | inferno-flamegraph > flamegraph.svg
```

Requires `perf_event_paranoid ≤ 1` or root (bench already runs as root for
kmod operations, so this works on this machine where paranoia=4).

### Output

Artifacts are saved to `bench-results/<hostname>/profile-<workload>-<scenario>/`:

- `bpftrace.txt` — raw bpftrace output: per-op latency histograms and call counts
- `io.json` — I/O amplification: logical vs actual bytes, amplification factor
- `perf.data` — (only with `--perf`) raw perf recording

A human-readable summary is printed to stdout:

```
Profile: write-files / rules-allow-all  (wall: 1.2s)

  agfs VFS ops (µs):
    op               calls    median   p99    total ms
    create            1000      45     120      48
    open              1001      15      35      16
    write_iter        1000      12      30      14
    lookup            3012       3      10       9
    cow_if_needed     1000       8      20       9
    permission        4024       1       3       5
    readdir             10      20      50       0.2
    d_revalidate      6000       0       1       1

  I/O amplification:
    logical writes:   4.0 MB
    actual writes:    5.2 MB
    factor:           1.30×
```

The `total ms` column ranks optimization targets directly: it is the product
of call count × mean latency and represents each op's contribution to total
wall time.

### Implementation structure

```
bench/src/profiler.rs        — Profiler: spawn/wait bpftrace, read /proc/self/io
bench/src/profile_report.rs  — format and save profile artifacts
```

The `Profiler` struct:

```rust
struct Profiler {
    bpftrace: Option<Child>,  // None if bpftrace unavailable
    output: TempPath,         // bpftrace writes stdout here
    io_before: ProcIo,
}

impl Profiler {
    fn start() -> Self           // spawn bpftrace, poll until "Attaching", snapshot io
    fn stop(self) -> ProfileData // SIGINT bpftrace, wait, parse output, diff io
}
```

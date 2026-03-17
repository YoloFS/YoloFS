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

Writes additionally incur staging costs: data is written to a staged inode
rather than directly to the base filesystem, and eventually
flushed to the base on `commit`.

The benchmark suite produces comprehensive, reproducible results demonstrating
agfs overhead across realistic workloads, and puts it in context by comparing
it against alternative staging/sandboxing approaches.

---

## 2. Workloads

The suite defines multiple workloads to avoid overfitting to a single access
pattern. Each workload is a self-contained Rust function that performs a
specific operation on the mounted filesystem. Workloads are categorised into
three kinds:

- **Session** (`--micro` / `--macro`): measure the full staging cycle
  (mount → work → commit). Report wall time decomposed into init, staging,
  and commit phases. Answer: "how long does an agfs session take?"
- **Op** (`--op`): measure per-operation throughput inside a single mounted
  session. Report IOPS, throughput, and latency percentiles. Answer: "what
  is the per-syscall overhead of agfs interposition?"

Workloads that need pre-existing files implement `populate_base()`. Each
backend calls this to populate the base directory *before* mounting, so that
operations correctly exercise copy-up / passthrough behaviour.

### Session microbenchmarks

Each session micro workload operates on 1,000 files of 4 KiB. The runner
measures the full lifecycle: mount → workload → commit.

| Workload | Operation | What it exercises |
|---|---|---|
| `write-files` | Create 1,000 new files | File creation + sequential write path |
| `read-files` | Read 1,000 existing files | Read passthrough (lower fs or staged inode) |
| `stat-files` | Stat 1,000 existing files | Metadata / permission check overhead |
| `overwrite-files` | Overwrite 1,000 existing files | Copy-on-write / copy-up path |
| `rename-files` | Rename 1,000 existing files | Directory ops + journal (agfs) or copy-up (overlayfs) |

### Session macrobenchmarks

#### Worktree (`worktree`)

Runs `git worktree add --detach` from a local Linux kernel clone
(`~/.cache/agfs-bench/linux`). Exercises the read-heavy path: the workload
reads thousands of objects from the base repository and writes a new working
tree into the mount. The fixture (initial clone) is constructed once and
reused; subsequent runs use `git worktree prune` to clean up stale entries
before each `worktree add`.

### Per-operation benchmarks

Op benchmarks measure per-syscall throughput and latency inside a mounted
session. The backend mounts once, the workload runs, and results are
self-reported by the subprocess (IOPS, MB/s, latency percentiles). No
init/commit timing is reported — the goal is to isolate the steady-state
overhead of the interposition layer.

#### I/O benchmarks (fio)

Large-file I/O using [fio](https://github.com/axboe/fio). Each workload
generates a jobfile, runs `fio --output-format=json`, and parses the result.
Buffered I/O (`direct=0`) is used because agfs operates at the VFS level and
real agent workloads use the page cache.

Read workloads come in **cold** and **warm** variants. Cold drops the page
cache via `sudo sh -c 'echo 3 > /proc/sys/vm/drop_caches'` before the run;
warm pre-reads the file so all I/O hits the page cache. This matters because warm-cache
reads isolate pure VFS interposition overhead, while cold-cache reads include
actual disk I/O amplified by interposition. Writes always go to the page
cache regardless, so they have no cold/warm split.

| Workload | Operation | Cache | What it measures |
|---|---|---|---|
| `fio-seq-read-cold` | Sequential 4K read, 1 GB | cold | Disk read + agfs lookup overhead |
| `fio-seq-read-warm` | Sequential 4K read, 1 GB | warm | Pure VFS interposition overhead |
| `fio-seq-write` | Sequential 4K write, 1 GB | — | Write path + staging overhead |
| `fio-rand-read-cold` | Random 4K read, 1 GB | cold | Random disk + agfs overhead |
| `fio-rand-read-warm` | Random 4K read, 1 GB | warm | Random read interposition overhead |
| `fio-rand-write` | Random 4K write, 1 GB | — | Random write + staging overhead |
| `fio-randrw-cold` | 70/30 read/write mix, 4K, 1 GB | cold | Mixed I/O, first-access pattern |
| `fio-randrw-warm` | 70/30 read/write mix, 4K, 1 GB | warm | Mixed I/O, steady-state |

fio must be installed (`apt install fio`). If absent, fio workloads are
skipped with a warning.

#### Metadata benchmarks (custom)

Small-file metadata operations, implemented in Rust. Each workload operates
on 10,000 files, records per-operation latency, and computes IOPS and
percentiles.

Read-path metadata ops (stat, readdir) have cold/warm variants for the same
reason as fio reads: cold stat hits disk for inode reads, warm stat is pure
dcache/icache. Write-path ops (create, rename, unlink) are always writes and
have no cold/warm split.

| Workload | Operation | Cache | What it measures |
|---|---|---|---|
| `meta-create` | Create 10,000 empty files | — | File creation throughput |
| `meta-append` | Append 4K to 10,000 files | — | Append + COW throughput |
| `meta-stat-cold` | Stat 10,000 files | cold | Inode read from disk |
| `meta-stat-warm` | Stat 10,000 files | warm | Dcache/icache lookup overhead |
| `meta-readdir-cold` | Readdir (10,000 entries) | cold | Directory read from disk |
| `meta-readdir-warm` | Readdir (10,000 entries) | warm | Cached directory listing overhead |
| `meta-rename` | Rename 10,000 files | — | Rename + journal overhead |
| `meta-unlink` | Unlink 10,000 files | — | Delete + journal overhead |

#### Op result model

Op workloads report per-operation metrics instead of wall time:

```
OpResult {
    iops:              f64,          // operations per second
    throughput_kbps:   Option<u64>,  // KB/s (fio workloads only)
    lat_us_p50:        f64,          // median latency in microseconds
    lat_us_p99:        f64,
    lat_us_p999:       f64,
}
```

#### Subprocess protocol for op workloads

The existing subprocess protocol (print `READY`, do work, exit) is extended.
Op workloads print a JSON results line after the work completes:

```
AGFS_BENCH_READY
<work happens>
AGFS_BENCH_RESULTS
{"iops": 125000, "throughput_kbps": 500000, "lat_us_p50": 3.2, "lat_us_p99": 18.5, "lat_us_p999": 142.0}
```

The parent checks `workload.kind()`: for `Op` workloads it parses the JSON
instead of measuring wall time.

#### Visualization

Op benchmarks run across all backends (native, agfs, overlayfs, branchfs).
A per-workload timeout (default 120 s) prevents FUSE-heavy backends from
blocking the entire suite indefinitely.

Op benchmarks are rendered as bar charts with backends on the x-axis and
IOPS on the y-axis. Native is shown as a baseline reference line. For fio
workloads, a secondary axis shows throughput (MB/s). Latency percentiles
(p50/p99) are shown in a table below each chart.

The report index page groups results into three sections: Session Micro,
Session Macro, and Per-Operation. Op benchmarks are included in the default
(no-flags) run alongside session benchmarks.

---

## 3. Backends

Each workload is run under multiple backends. A backend defines how writes are
staged and committed. The goal is to isolate the cost of each mechanism and
place agfs in context relative to alternatives.

| Backend | Mechanism | Needs root? | Default? |
|---|---|---|---|
| `native` | Direct ext4 writes, no staging | no | yes |
| `agfs-allow-all` | Kernel stackable fs; `allow-rw /` rule | no (setuid) | yes |
| `agfs-realistic` | Kernel stackable fs; workload-defined rules | no (setuid) | yes |
| `overlayfs` | User-namespace overlayfs; replay upper on commit | no (user-ns) | yes |
| `branchfs` | FUSE copy-on-write branches; `branchfs commit` | no | yes |
| `try` | Shell wrapper around overlayfs (`try` tool) | no (user-ns) | **hidden** |
| `btrfs` | btrfs subvolume checkpoint; rsync back on commit | yes (cap) | not yet |

`agfs-bench` does **not** need to run as root. The agfs binary is setuid,
overlayfs and `try` use user namespaces, and branchfs runs in userspace. Only
the profiler (§7) invokes `sudo` internally for `perf` and `bpftrace`.

### agfs backends

The agfs backend is split into two configurations to isolate the cost of each
level of gating:

| Backend | Configuration | What it measures |
|---|---|---|
| `agfs-allow-all` | `allow-rw /` rule | VFS interposition + staging; no per-access gating |
| `agfs-realistic` | workload-defined rules | Typical rule-based config; most accesses hit cache |

`agfs-allow-all` is the practical floor for a useful agfs configuration.
`native` is the absolute floor.

### overlayfs

The `overlayfs` backend uses Linux overlayfs directly, without any wrapper
tool. Each iteration:

1. Creates a fresh tempdir with `lower/`, `upper/`, `work/`, `merged/`.
2. Enters a user + mount namespace via `unshare(1)`.
3. Mounts overlayfs (`mount -t overlay … -o userxattr`).
4. Runs the workload inside the merged directory.
5. Commits by replaying the upper dir onto lower: regular files are renamed
   (O(1) on the same filesystem), whiteout devices (char 0,0) trigger
   deletions, opaque directories (`user.overlay.opaque` xattr) replace their
   lower counterparts, and symlinks are recreated.

This gives a clean measurement of overlayfs overhead without shell noise.

### try (hidden)

`try` is a shell script that wraps overlayfs in a user namespace. It is
**hidden by default** because its shell-based setup (forking ~90 subprocesses,
exec'ing ~130 commands per invocation) adds ~400ms of overhead unrelated to the
staging mechanism being measured. The `overlayfs` backend measures the same
underlying mechanism without this noise.

To include `try` in a run: `agfs-bench --backend try`.

The adapter uses a self-exec pattern: it invokes
`try -n -D <sandbox> -- agfs-bench exec-workload …` (no auto-commit), then
calls `try commit <sandbox>` as the commit step.

### branchfs

`branchfs` is a FUSE filesystem (from `bench/third_party/branchfs`) that provides
O(1) branch creation and atomic commit-to-parent semantics. Each iteration:

1. Mounts branchfs over a fresh base directory with a per-iteration storage
   directory (`branchfs mount --base <base> --storage <storage> <mnt>`).
2. Creates a `bench` branch (`branchfs create bench <mnt>`).
3. Runs the workload inside the mount (via `exec-workload` subprocess).
4. Commits the branch (`branchfs commit <mnt>`).
5. Unmounts (`branchfs unmount <mnt>`).

### btrfs

**Not yet implemented.** Design:

btrfs subvolume checkpoints are O(1) copy-on-write clones within a btrfs volume.
Because the root filesystem is ext4, btrfs requires a dedicated raw disk
provided by the user (e.g. `/dev/sdb`). The bench tool would handle all setup
automatically and idempotently:

1. **`mkfs`**: if the device does not already contain a btrfs filesystem,
   `mkfs.btrfs` is run once.
2. **Mount**: if `/mnt/btrfs-bench` is not already mounted, the device is
   mounted there.
3. **Base subvolume**: if `/mnt/btrfs-bench/<workload>/base` does not exist,
   it is created as a btrfs subvolume and the fixture is copied into it.

The device would be specified via `--btrfs-device <path>`. If the flag is
omitted the btrfs backend is skipped.

Each iteration:
1. Takes an O(1) checkpoint of `base` → `work`.
2. Runs the workload inside the checkpoint.
3. On commit, syncs changes back to `base` via rsync and deletes the checkpoint.

---

## 4. Measurement Models

### Session workloads (micro / macro)

Time is decomposed into three phases:

```
total = init_time + staging_time + commit_time
```

- **`init_time`**: wall time of sandbox creation (mount, checkpoint, namespace
  setup). This is the cost of *entering* the sandbox before any work begins.
  For `native` this is None.
- **`staging_time`**: wall time of the workload itself. This is what the agent
  experiences while doing work.
- **`commit_time`**: wall time of the commit step. For `native` this is None.

| Backend | init | staging | commit |
|---|---|---|---|
| `native` | — | workload | — |
| `agfs-*` | `agfs mount` | workload | `agfs commit` |
| `overlayfs` | `unshare` + `mount -t overlay` | workload | replay upper → lower |
| `try` | shell namespace setup | workload | `try commit` |
| `branchfs` | `branchfs mount` + `create` | workload | `branchfs commit` |
| `btrfs` | `btrfs subvolume checkpoint` | workload | rsync + delete |

Every backend runs the workload as a subprocess via the `exec-workload`
subcommand. The subprocess prints a `READY` marker to stdout just before it
starts the workload. The parent watches for this marker — wall time before it
arrives is startup overhead (process spawn, or for `try`/`overlayfs`, full
namespace + overlayfs setup), wall time after is staging. For backends with a
separate init step (agfs, branchfs), init is measured in the parent before
spawning the subprocess; for `try` and `overlayfs`, init *is* the startup time
reported by the subprocess protocol.

All timings are taken with `std::time::Instant` inside the bench binary.
Each (workload, backend) pair is run `--runs N` times (default 3), preceded
by one warm-up run; mean ± stddev are reported, and outliers (>2σ) are flagged.
Each iteration prints its result inline:

```
    iter 1/3… 489 ms  (init 5 + stage 389 + commit 95)
```

### Op workloads (per-operation)

Op workloads are **self-timing**: the subprocess measures its own metrics and
reports them as JSON. The parent orchestrates backend setup/teardown but does
not measure wall time.

Reported metrics:
- **IOPS** — operations per second.
- **Throughput** (KB/s) — for I/O workloads only.
- **Latency percentiles** — p50, p99, p99.9 in microseconds.

For fio workloads, these come directly from fio's JSON output. For metadata
workloads, the subprocess records a `Vec<Duration>` of per-op latencies and
computes IOPS = count / total_time, with percentiles from the sorted vector.

Each (workload, backend) pair is still run `--runs N` times. Mean ± stddev
of IOPS across iterations is reported.

```
    iter 1/3… 124,502 IOPS  (p50 3.2 µs, p99 18.5 µs)
```

---

## 5. Fixture vs Run

**Fixture** (setup, not timed): constructed once and reused across all
subsequent runs. If the fixture already exists it is not rebuilt.

- Each workload declares its own fixture requirements via `ensure_fixture()`,
  called once before any backends run for that workload.
- `worktree`: clones the Linux kernel to `~/.cache/agfs-bench/linux`.
- `write-files`: no external fixture needed.

**Warm-up**: one warm-up run is performed in `native` mode before all backends
for a workload begin. It populates the page cache and warms dentry/inode caches.
The warm-up result is discarded.

**Run** (timed): each backend runs N timed iterations. Each iteration creates
a fresh session (tempdir / mount / checkpoint) to avoid stale dentry state.
Mean ± stddev of the N timed iterations is reported; outliers (>2σ) are flagged.

**Teardown**: the mount / checkpoint / session directory are removed automatically
when the session is dropped at the end of each iteration.

---

## 6. Implementation

The benchmark suite is a Rust binary (`agfs-bench`) in the same Cargo workspace
as the CLI, under `bench/src/`. It shares ioctl types, mount helpers, config
parsing, and kmsg utilities with the CLI via the library crate.

### Directory layout

```
bench/src/
  main.rs          — CLI, backend runner, statistics, exec-workload subcommand
  backend.rs       — Backend trait + exec-workload subprocess helper
  backends/
    mod.rs         — registry (all, by_name)
    native.rs
    agfs.rs        — agfs-allow-all + agfs-realistic + ProfileSession
    overlayfs.rs   — direct overlayfs in user namespace
    try_backend.rs — try shell wrapper (hidden)
    branchfs.rs
  workload.rs      — Workload trait + IterResult
  workloads/       — one file per workload
  profiler.rs      — bpftrace + perf flamegraph
  report.rs        — plotly HTML report
```

### Backend availability and visibility

Each backend implements `available()`, `unavailable_reason()`, and `hidden()`.

- **Unavailable** backends are missing required tools; they are always skipped.
- **Hidden** backends are functional but excluded from default runs because
  they add noise (e.g. `try`'s shell overhead dominates the overlayfs cost it
  aims to measure). Use `--backend <name>` to run them explicitly.

`agfs-bench list` shows all backends with their status.

### Third-party tools

| Tool | Source | Install |
|---|---|---|
| `try` | `bench/third_party/try/` | `make -C bench install-try` |
| `branchfs` | `bench/third_party/branchfs/` | `make -C bench install-branchfs` |

### CLI

```
agfs-bench [--workload <name>] [--backend <name>] [--micro] [--macro] [--op]
           [--runs N] [--verbose] [--timestamped-results]
agfs-bench rerender
agfs-bench list
agfs-bench profile [--workload <name>] [--scenario <name>] [--no-bpftrace]
agfs-bench exec-workload --name <name> --dest <path> [--verbose]
```

- With no flags: runs all workloads × all available non-hidden backends.
- `--micro` / `--macro` / `--op`: run only session micro, session macro, or
  per-operation benchmarks respectively.
- `--workload` / `--backend`: filter to a specific combination. `--backend`
  overrides hidden status, so `--backend try` will run `try`.
- `--runs N`: number of timed iterations (default 3).
- `--verbose`: capture detailed logs for all runs, not just failures.
- `--timestamped-results`: write results into a timestamped subdirectory
  (`results-bench/<hostname>/<timestamp>/`) instead of overwriting.
- `rerender`: regenerate HTML reports from existing `results.json`.
- `list`: print all registered workloads and backends with availability.
- `profile`: run the profiling mode (see §7).
- `exec-workload`: internal subcommand used by all backends to run a
  workload as a subprocess. Prints a `READY` marker to stdout before the
  workload starts, enabling the parent to split init from staging time.

### Logging and failure handling

On failure, the failing (workload, backend) combination is automatically rerun
with verbose logging enabled. Verbose logs include:

- Workload stdout/stderr
- agfs journal contents at the point of failure (agfs backend only)

### Results

Results are written to `results-bench/<hostname>/`. By default the previous
result for that host is overwritten; pass `--timestamped-results` to retain
multiple runs.

Each result records environment metadata (CPU, memory, storage device and model,
filesystem type, kernel version, distro) so results from different machines are
not conflated. Running `--workload X` or `--backend Y` merges only the
re-run entries into the existing `results.json`, preserving results for
workloads and backends that were not part of the current run.

An HTML report (`report-<workload>.html`) is generated per workload using the
[`plotly`](https://crates.io/crates/plotly) crate:

- **Session workloads**: stacked bar charts showing backend × (init, staging,
  commit) time. Native rendered as a bar and as a reference line. Error bars
  showing total stddev across iterations.
- **Op workloads**: bar charts with backends on the x-axis and IOPS on the
  y-axis. For fio workloads, throughput (MB/s) on a secondary axis. Latency
  percentiles (p50/p99) in a table below each chart. Native as baseline
  reference line.

The index page groups results into three sections: Session Micro, Session
Macro, and Per-Operation.

---

## 7. Profiling

`agfs-bench profile` identifies *where* agfs overhead goes. It runs a single
iteration (no warmup, no averaging) with profiling tools active. Only the agfs
backend is profiled (the other backends are not kernel-instrumented).

### bpftrace op latency histograms

A bpftrace script runs alongside the workload, instrumenting these agfs hot-path
kfunctions via BTF (`kfunc`/`kretfunc` probes):

| Function | What it covers |
|---|---|
| `agfs_lookup` | Dentry resolution (every path component) |
| `agfs_d_revalidate` | Dentry cache validation |
| `agfs_permission` | Permission check |
| `agfs_resolve_perm` | Rule match + inode cache lookup/store |
| `agfs_open` | File open |
| `agfs_create` | File creation |
| `agfs_create_staged` | Staging entry allocation for new file |
| `agfs_read_iter` | Read path (lower fs or staged inode) |
| `agfs_write_iter` | Write path (always to staged inode) |
| `agfs_do_cow` | Copy-on-write execution (at open time) |
| `agfs_staging_alloc` | Inode allocation in inode store |
| `agfs_readdir` | Directory listing merged from base + staging |
| `agfs_journal_append_a` | Journal write for add |
| `agfs_journal_append_d` | Journal write for delete |
| `agfs_journal_append_r` | Journal write for rename |
| `agfs_release` | File release |
| `agfs_find_dirent` | Staging index lookup |

Each function gets its own per-tid start map (`@s_<func>[tid]`) to avoid
clobbering timestamps on nested calls (e.g. `agfs_create` calling
`agfs_staging_alloc`). Latency is accumulated into a `hist()` map in
microseconds; the map is flushed on SIGINT when the workload completes.

perf is spawned first so it is already recording before bpftrace begins
attaching probes. bpftrace signals readiness via `BEGIN { printf("READY\n"); }`;
the workload starts only after READY is received.

Pass `--no-bpftrace` to skip the histogram collection and get a clean flamegraph
without BPF ring-buffer overhead in the stacks.

### Flamegraph

`perf record -g -F 99 -p <self-pid>` runs for the duration of the workload.
The resulting `perf.data` is processed via the `inferno` crate to produce:

- `stacks.txt` — collapsed stack text. Diffable across runs, greppable.
- `flamegraph.svg` — interactive SVG. Open in a browser to zoom into hot paths.

Both tools are invoked via `sudo` internally; the bench binary itself does not
need to run as root.

### Output

Artifacts are saved to `results-bench/<hostname>/profiling/<workload>/<scenario>/`:

- `summary.txt` — ranked op table (printed to stdout and saved)
- `bpftrace.txt` — raw per-op latency histograms
- `probe.bt` — the generated bpftrace script
- `stacks.txt` — collapsed perf stacks
- `flamegraph.svg` — interactive flamegraph

Example summary:

```
Profile: write-files / agfs-allow-all  (wall: 167 ms)

  op                               calls  median µs  p99 µs    total ms
  --------------------------------------------------------------------------
  create                            1000         16    1024       100.5
  create_staged                     1000         16    1024       100.1
  staging_alloc                     1000         16      64        27.6
  lookup                            1000          8      32        15.2
  write_iter                        1000          4      32         7.4
  open                              1000          2       8         2.5
  journal_append_a                  1000          1       8         1.6
```

The `total ms` column ranks optimization targets by contribution to wall time.

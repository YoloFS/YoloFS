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
tree into the mount. The fixture (initial clone) is constructed once and
reused; subsequent runs use `git worktree prune` to clean up stale entries
before each `worktree add`.

---

## 3. Backends

Each workload is run under multiple backends. A backend defines how writes are
staged and committed. The goal is to isolate the cost of each mechanism and
place agfs in context relative to alternatives.

| Backend | Mechanism | Needs root? |
|---|---|---|
| `native` | Direct ext4 writes, no staging | no |
| `agfs` | Kernel stackable fs; inode store + `agfs commit` | no (setuid) |
| `try` | overlayfs sandbox via `unshare`; `try commit` to apply | no (user-ns) |
| `branchfs` | FUSE copy-on-write branches; `branchfs commit` | no |
| `btrfs` | btrfs subvolume snapshot; rsync back on commit | yes (cap) |

### agfs scenarios

The agfs backend is run under three permission configurations to isolate the
cost of each level of gating:

| Scenario | Configuration | What it measures |
|---|---|---|
| `agfs-allow-all` | `allow-rw /` rule | VFS interposition + staging; no per-access gating |
| `agfs-realistic` | workload-defined rules | Typical rule-based config; most accesses hit cache |

`agfs-allow-all` is the practical floor for a useful agfs configuration.
`native` is the absolute floor.

### try

`try` wraps a command in a Linux user-namespace with an overlayfs upper layer,
then offers to commit or discard the changes. The adapter invokes the workload
binary as a subprocess under `try -n -- <cmd>` (no auto-commit), then calls
`try commit <sandbox_dir>` as the commit step, which replays the overlay upper
layer back to the base directory.

### branchfs

`branchfs` is a FUSE filesystem (from `third_party/branchfs`) that provides
O(1) branch creation and atomic commit-to-parent semantics. The adapter mounts
branchfs over the base directory, creates a new branch, runs the workload
inside it, then calls `branchfs commit`.

### btrfs

btrfs subvolume snapshots are O(1) copy-on-write clones within a btrfs volume.
Because the root filesystem is ext4, btrfs requires a dedicated raw disk
provided by the user (e.g. `/dev/sdb`). The bench tool handles all setup
automatically and idempotently:

1. **`mkfs`**: if the device does not already contain a btrfs filesystem,
   `mkfs.btrfs` is run once.
2. **Mount**: if `/mnt/btrfs-bench` is not already mounted, the device is
   mounted there.
3. **Base subvolume**: if `/mnt/btrfs-bench/<workload>/base` does not exist,
   it is created as a btrfs subvolume and the fixture is copied into it.

The device is specified via `--btrfs-device <path>` (e.g. `--btrfs-device
/dev/sdb`). If the flag is omitted the btrfs backend is skipped silently.

Each iteration:
1. Takes an O(1) snapshot of `base` → `work`.
2. Runs the workload inside the snapshot.
3. On commit, syncs changes back to `base` via rsync and deletes the snapshot.

All setup steps are guarded so re-running on an already-prepared disk skips
`mkfs` and `mount` safely.

---

## 4. Timing Model

Time is decomposed into two phases:

```
total = staging_time + commit_time
```

- **`staging_time`**: wall time of the workload itself. This is what the agent
  experiences while doing work.
- **`commit_time`**: wall time of the commit step. For `native` this is zero.

All timings are taken with `std::time::Instant` inside the bench binary.
Each (workload, backend) pair is run `--runs N` times (default 3), preceded
by one warm-up run; mean ± stddev are reported, and outliers (>2σ) are flagged.
Each iteration prints its result inline:

```
    iter 1/3… 412 ms  (stage 387 + commit 25)
```

---

## 5. Fixture vs Run

**Fixture** (setup, not timed): constructed once and reused across all
subsequent runs. If the fixture already exists it is not rebuilt.

- Each workload declares its own fixture requirements via `ensure_fixture()`,
  called once before any backends run for that workload.
- `worktree`: clones the Linux kernel to `~/.cache/agfs-bench/linux`.
- `write-files`: no external fixture needed.
- For the btrfs backend, fixtures are additionally mirrored to
  `/mnt/btrfs-bench/<workload>/base` before the first btrfs run.

**Warm-up**: one warm-up run is performed in `native` mode before all backends
for a workload begin. It populates the page cache and warms dentry/inode caches.
The warm-up result is discarded.

**Run** (timed): each backend runs N timed iterations. Each iteration creates
a fresh session (tempdir / mount / snapshot) to avoid stale dentry state.
Mean ± stddev of the N timed iterations is reported; outliers (>2σ) are flagged.

**Teardown**: the mount / snapshot / session directory are removed automatically
when the session is dropped at the end of each iteration.

---

## 6. Implementation

The benchmark suite is a Rust binary (`agfs-bench`) in the same Cargo workspace
as the CLI, under `bench/src/`. It shares ioctl types, mount helpers, config
parsing, and klog utilities with the CLI via the library crate.

### Directory layout

```
bench/src/
  main.rs          — CLI, scenario runner, statistics
  workload.rs      — Workload trait + IterResult
  workloads/       — one file per workload
  backend.rs       — Backend + Session traits
  backends/
    native.rs
    agfs.rs
    try.rs
    branchfs.rs
    btrfs.rs
  profiler.rs      — bpftrace + perf flamegraph
  report.rs        — plotly HTML report
```

### Third-party tools

| Tool | Source | Build |
|---|---|---|
| `try` | `third_party/try/` | shell script, no build |
| `branchfs` | `third_party/branchfs/` | `cargo build --release` |
| `btrfs-progs` | system package | `apt install btrfs-progs` |

`make install` builds and installs all of the above. btrfs disk setup is
handled automatically by `agfs-bench` at runtime when `--btrfs-device` is
passed; no separate `make` target is needed.

### CLI

```
agfs-bench [--workload <name>] [--backend <name>] [--runs N] [--verbose]
           [--timestamped-results] [--btrfs-device <path>]
agfs-bench rerender
agfs-bench list
agfs-bench profile [--workload <name>] [--scenario <name>] [--no-bpftrace]
```

- With no flags: runs all workloads × all backends (btrfs skipped unless
  `--btrfs-device` is given).
- `--workload` / `--backend`: filter to a specific combination.
- `--runs N`: number of timed iterations (default 3).
- `--btrfs-device <path>`: raw block device to use for the btrfs backend
  (e.g. `/dev/sdb`). The bench tool formats, mounts, and prepares it
  automatically on first use; subsequent runs skip steps already done.
- `--verbose`: capture detailed logs for all runs, not just failures.
- `--timestamped-results`: write results into a timestamped subdirectory
  (`results-bench/<hostname>/<timestamp>/`) instead of overwriting.
- `rerender`: regenerate HTML reports from existing `results.json`.
- `list`: print all registered workload names.
- `profile`: run the profiling mode (see §7).

### Logging and failure handling

On failure, the failing (workload, backend) combination is automatically rerun
with verbose logging enabled. Verbose logs include:

- Workload stdout/stderr
- Kernel messages captured via `klog::snapshot` / `klog::since`
- agfs journal contents at the point of failure (agfs backend only)

### Results

Results are written to `results-bench/<hostname>/`. By default the previous
result for that host is overwritten; pass `--timestamped-results` to retain
multiple runs.

Each result records environment metadata (CPU, memory, storage device and model,
filesystem type, kernel version, distro) so results from different machines are
not conflated. Running `--workload X` merges only that workload's results into
the existing `results.json`.

An HTML report (`report-<workload>.html`) is generated per workload using the
[`plotly`](https://crates.io/crates/plotly) crate:

- Stacked bar charts: backend × (staging time, commit time).
- Native rendered as a bar and as a reference line across other backends.
- Error bars showing total stddev across iterations.

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
| `agfs_find_override` | Staging index lookup |

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
Profile: write-files / rules-allow-all  (wall: 167 ms)

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

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
| `agfs-allow-all` | Kernel stackable fs; `allow-rw /` rule | no (setuid) |
| `agfs-realistic` | Kernel stackable fs; workload-defined rules | no (setuid) |
| `try` | overlayfs sandbox via `unshare`; `try commit` to apply | no (user-ns) |
| `branchfs` | FUSE copy-on-write branches; `branchfs commit` | no |
| `btrfs` | btrfs subvolume snapshot; rsync back on commit | yes (cap) |

`agfs-bench` does **not** need to run as root. The agfs binary is setuid,
`try` uses user namespaces, and branchfs runs in userspace. Only the profiler
(§7) invokes `sudo` internally for `perf` and `bpftrace`.

### agfs backends

The agfs backend is split into two configurations to isolate the cost of each
level of gating:

| Backend | Configuration | What it measures |
|---|---|---|
| `agfs-allow-all` | `allow-rw /` rule | VFS interposition + staging; no per-access gating |
| `agfs-realistic` | workload-defined rules | Typical rule-based config; most accesses hit cache |

`agfs-allow-all` is the practical floor for a useful agfs configuration.
`native` is the absolute floor.

### try

`try` wraps a command in a Linux user-namespace with an overlayfs upper layer,
then offers to commit or discard the changes. Since the workload runs inside
`try`'s namespace, the adapter uses a self-exec pattern: it invokes
`try -n -D <sandbox> -- agfs-bench exec-workload --name <name> --dest <dest>`
(no auto-commit), then calls `try commit <sandbox>` as the commit step.

`try` requires overlayfs to work inside user namespaces. Availability is
probed at startup by running `try -n -- /bin/true`; if this fails (e.g.
stale submounts under `/tmp`, or a kernel that rejects unprivileged
overlayfs), the backend is skipped with a diagnostic message.

### branchfs

`branchfs` is a FUSE filesystem (from `third_party/branchfs`) that provides
O(1) branch creation and atomic commit-to-parent semantics. Each iteration:

1. Mounts branchfs over a fresh base directory with a per-iteration storage
   directory (`branchfs mount --base <base> --storage <storage> <mnt>`).
2. Creates a `bench` branch (`branchfs create bench <mnt>`).
3. Runs the workload directly inside the mount.
4. Commits the branch (`branchfs commit <mnt>`).
5. Unmounts (`branchfs unmount <mnt>`).

### btrfs

**Not yet implemented.** Design:

btrfs subvolume snapshots are O(1) copy-on-write clones within a btrfs volume.
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
1. Takes an O(1) snapshot of `base` → `work`.
2. Runs the workload inside the snapshot.
3. On commit, syncs changes back to `base` via rsync and deletes the snapshot.

---

## 4. Timing Model

Time is decomposed into three phases:

```
total = init_time + staging_time + commit_time
```

- **`init_time`**: wall time of sandbox creation (mount, snapshot, namespace
  setup). This is the cost of *entering* the sandbox before any work begins.
  For `native` this is None.
- **`staging_time`**: wall time of the workload itself. This is what the agent
  experiences while doing work.
- **`commit_time`**: wall time of the commit step. For `native` this is None.

| Backend | init | staging | commit |
|---|---|---|---|
| `native` | — | workload | — |
| `agfs-*` | `agfs mount` | workload | `agfs commit` |
| `try` | namespace + overlay setup | workload | `try commit` |
| `branchfs` | `branchfs mount` + `create` | workload | `branchfs commit` |
| `btrfs` | `btrfs subvolume snapshot` | workload | rsync + delete |

For `try`, the init/staging split is measured via a ready signal: the
`exec-workload` subprocess prints a marker to stdout just before it starts the
workload. The parent watches for this marker — wall time before it arrives is
init (namespace + overlayfs setup), wall time after is staging.

All timings are taken with `std::time::Instant` inside the bench binary.
Each (workload, backend) pair is run `--runs N` times (default 3), preceded
by one warm-up run; mean ± stddev are reported, and outliers (>2σ) are flagged.
Each iteration prints its result inline:

```
    iter 1/3… 489 ms  (init 5 + stage 389 + commit 95)
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
a fresh session (tempdir / mount / snapshot) to avoid stale dentry state.
Mean ± stddev of the N timed iterations is reported; outliers (>2σ) are flagged.

**Teardown**: the mount / snapshot / session directory are removed automatically
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
  backend.rs       — Backend trait
  backends/
    mod.rs         — registry (all, by_name)
    native.rs
    agfs.rs        — agfs-allow-all + agfs-realistic + ProfileSession
    try_backend.rs — try (self-exec via exec-workload)
    branchfs.rs
  workload.rs      — Workload trait + IterResult
  workloads/       — one file per workload
  profiler.rs      — bpftrace + perf flamegraph
  report.rs        — plotly HTML report
```

### Backend availability

Each backend implements `available()` and `unavailable_reason()`. At startup,
unavailable backends are skipped with a diagnostic:

```
Skipping backend 'try': 'try -n -- /bin/true' failed (stale mounts under /tmp? overlayfs issue?)
```

`agfs-bench list` shows all backends with availability status.

### Third-party tools

| Tool | Source | Install |
|---|---|---|
| `try` | `third_party/try/` | `make install-third-party` |
| `branchfs` | `third_party/branchfs/` | `make install-third-party` |

### CLI

```
agfs-bench [--workload <name>] [--backend <name>] [--runs N] [--verbose]
           [--timestamped-results]
agfs-bench rerender
agfs-bench list
agfs-bench profile [--workload <name>] [--scenario <name>] [--no-bpftrace]
agfs-bench exec-workload --name <name> --dest <path> [--verbose]
```

- With no flags: runs all workloads × all available backends.
- `--workload` / `--backend`: filter to a specific combination.
- `--runs N`: number of timed iterations (default 3).
- `--verbose`: capture detailed logs for all runs, not just failures.
- `--timestamped-results`: write results into a timestamped subdirectory
  (`results-bench/<hostname>/<timestamp>/`) instead of overwriting.
- `rerender`: regenerate HTML reports from existing `results.json`.
- `list`: print all registered workloads and backends with availability.
- `profile`: run the profiling mode (see §7).
- `exec-workload`: internal subcommand used by the `try` backend to run a
  workload as a subprocess inside the `try` namespace.

### Logging and failure handling

On failure, the failing (workload, backend) combination is automatically rerun
with verbose logging enabled. Verbose logs include:

- Workload stdout/stderr
- Kernel messages captured via `kmsg::KmsgCursor`
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

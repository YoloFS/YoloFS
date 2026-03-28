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

### Publication artifacts

The report pipeline can emit a publication-oriented **data-op (fio)** summary
table under `results-bench/<host>/paper/`:

- `op-data-summary.tex` — compact LaTeX table source,
- `op-data-summary.pdf` — compiled table PDF (if `pdflatex` is available),
- `op-data-summary.svg` — vector SVG rendered from the PDF (if `pdftocairo`
  is available).

The table reports throughput in MB/s and compares each backend to native. If
the difference from native is under 5%, the cell is rendered as
`same as baseline`.

A dedicated `paper-report.html` page links these artifacts.

---

## 2. Workloads

The suite defines multiple workloads to avoid overfitting to a single access
pattern. Each workload is a self-contained Rust function that performs a
specific operation on the mounted filesystem. Workloads are organised in the
report into three families:

- **Per-op micro-benchmark** (`--op`): one mounted session, then repeated
  operations. Reports IOPS, throughput, and latency percentiles. Shown in two
  sections: **Big file data operations** (`fio-*`) and **Small file metadata
  operations** (`meta-*`).
- **Session micro-benchmark** (`--micro`): full staging lifecycle for small,
  single-kind operations. Reports init, staging (run), and commit time. This
  family now includes both one-shot micro workloads and checkpoint-scalability
  workloads.
- **Session macro-benchmark** (`--macro`): full staging lifecycle for larger,
  realistic tasks.

Workloads that need pre-existing files implement `populate_base()`. Each
backend calls this to populate the base directory *before* mounting, so that
operations correctly exercise copy-up / passthrough behaviour.
Workload-specific report metadata lives beside each workload implementation,
and the report source path is derived from Rust's `file!()` macro rather than
hand-maintained path strings. For Rust-driven workloads, the report execution
snippet is captured from the same `macro_rules!` input that defines the actual
execution function body, so the displayed code stays mechanically aligned with
what `run()` really calls.

### Session micro-benchmarks

Each session micro workload operates on 1,000 files of 4 KiB. The runner
measures the full lifecycle: mount → workload → commit.

| Workload | Operation | What it exercises |
|---|---|---|
| `write-files` | Create 1,000 new files | File creation + sequential write path |
| `read-files` | Read 1,000 existing files | Read passthrough (lower fs or staged inode) |
| `stat-files` | Stat 1,000 existing files | Metadata / permission check overhead |
| `overwrite-files` | Overwrite 1,000 existing files | Copy-on-write / copy-up path |
| `rename-files` | Rename 1,000 existing files | Directory ops + journal (agfs) or copy-up (overlayfs) |

#### Checkpoint scalability session benchmarks

This benchmark line measures how operation latency evolves as checkpoint depth
grows within one session. The goal is to answer: "does lookup/COW/journal cost
drift as the number of checkpoints increases?"

High-level loop for one timed run:

1. Start a fresh mounted session with an initial working set.
2. For checkpoint index `k = 1..K`:
   - run a fixed operation batch,
   - record operation latencies for that batch,
   - take a checkpoint,
   - carry resulting filesystem state into the next `k`.

Candidate operation batches per checkpoint:

- `stat` over existing files,
- `readdir` over existing directories,
- `unlink` on a slice of existing files,
- `read` on existing files,
- `create` on new files,
- `overwrite` on existing files.

Default per-checkpoint sample counts for this family:

- `readdir`: 1 operation per checkpoint step,
- all other operations listed above: 10 operations per checkpoint step.

Checkpoint depth is configurable via `AGFS_BENCH_CHECKPOINT_STEPS` (default:
`100`).

To avoid a shrinking or steady-state-only dataset, the benchmark enforces a
net growth policy: each checkpoint step must add more files than it removes.
For example, if a step unlinks `N`, it creates at least `N + delta` new files.
This keeps inode and directory population increasing with checkpoint number and
prevents late checkpoints from becoming trivially small.

Reported outputs should include:

- per-checkpoint latency statistics (mean/p50/p99) for each operation batch,
- optional throughput per checkpoint,
- visualization axes: x = checkpoint number (`k`), y = average latency per
  operation (microseconds),
- visualization style: faceted multiline line plots (one facet per operation,
  one line per backend within each facet),
- final file-count trajectory to verify the growth invariant held.

Design notes:

- Keep cache policy explicit (separate warm and cold variants where relevant).
- Use one shared implementation parameterized by operation mix and growth
  policy; avoid duplicating workload bodies per operation.
- For comparability, fix batch sizes per checkpoint and checkpoint count `K`
  per workload profile.
- Checkpoint creation time itself should be recorded as its own series in
  addition to operation latency, so operation drift and checkpoint drift are
  separable.

### Session macro-benchmarks

#### Developer workflow (`dev-workflow`)

Emulates a realistic multi-step kernel development session: creating a
worktree, building, then iterating through a patchset with search/edit/
build/commit cycles. This is the flagship macro benchmark — it exercises
the full agfs lifecycle under a realistic agent-like workload.

**Fixture**: Reuses the linux git clone from the `worktree` workload
(`~/.cache/agfs-bench/linux`) as the source repository and object store.
The timed workload does **not** start from whatever commit that clone
happens to be on. Instead, it always creates a detached worktree at the
exact pinned base commit for the chosen series, so the scripted edits are
defined against a stable tree.

**Patchset**: A real Linux kernel patchset, extracted from git history and
replayed as an explicit developer workflow, not via `git apply`. The
benchmark ships checked-in search/edit/build/commit scripts for each patch
(no network during the run). Requirements for the chosen series:

- 5–10 commits from a single subsystem (ext4, overlayfs, or VFS)
- Touches files in `fs/` (and possibly `include/linux/`)
- Each intermediate commit compiles with tinyconfig
- Has a clearly defined upstream base commit that can be checked out exactly
- Each step can be reproduced against that base with grep/sed/git commands

**Chosen series**: Amir Goldstein's overlayfs `ovl_file` refactoring
(5 commits, merged in v6.13). The workload starts from the parent of commit 1,
not from a local v6.12.x tree:

- Base commit: `87a8a76c34a2^`

1. `87a8a76c34a2` — allocate container struct `ovl_file` for ovl private context
2. `18e48d0e2c7b` — store upper real file in `ovl_file` struct
3. `c2c54b5f34f6` — do not open non-data lower file for fsync
4. `4333e42ed444` — convert `ovl_real_fdget_path()` callers to `ovl_real_file_path()`
5. `d66907b51ba0` — convert `ovl_real_fdget()` callers to `ovl_real_file()`

4 of 5 commits touch only `fs/overlayfs/file.c`; commit 1 also
touches `dir.c` and `overlayfs.h`. Total: ~210 insertions, ~150
deletions. The benchmark stores checked-in workflow fixtures under
`bench/fixtures/dev-workflow/`: pinned commit IDs, per-commit grep
commands, per-commit `sed -i` edit scripts, and commit messages.

Each commit follows the same developer/agent loop:
1. **Search** — `grep -rn` for relevant patterns (e.g., find all call
   sites of a function, locate struct definitions)
2. **Edit** — `sed -i` to make the changes (reproducing what the real
   commit does, derived from studying the actual diff)
3. **Build** — `make -j$(nproc)` incremental build
4. **Commit** — `git status` → `git diff` → `git add <files>` → `git commit -m "..."`

The real upstream patches serve as reference for what edits to make, but the
workload executes checked-in grep + sed scripts (not `git apply`) to emulate
how an agent would actually work: search for context, transform the checked-out
base tree, build, inspect the diff, and commit. This avoids version drift:
the scripts are authored once against the exact pinned base commit and the
workload always materializes that same starting tree before timing begins.

**Steps measured** (each timed independently):

| Step | What | Runs once or per-patch |
|------|------|------------------------|
| `worktree` | `git worktree add --detach <dest> <base-commit>` | once |
| `config` | `make tinyconfig` | once |
| `initial-build` | `make -j$(nproc)` from clean | once |
| `search` | `grep -rn <pattern> fs/` | per-patch |
| `edit` | `sed -i` commands reproducing the commit | per-patch |
| `incremental-build` | `make -j$(nproc)` after edit | per-patch |
| `git-status` | `git status` | per-patch |
| `git-diff` | `git diff` | per-patch |
| `git-add` | `git add <files>` | per-patch |
| `git-commit` | `git commit -m "..."` | per-patch |
| `checkpoint` | backend checkpoint after each commit | per-patch |

For agfs, each `git commit` is followed by an agfs checkpoint. For
overlayfs/branchfs, the equivalent checkpoint mechanism is used. For
native, checkpoints are no-ops.

**Output**: A JSON series with per-step timings. The workload also records
total wall time and the number of files changed per commit.

**Paper plot**: Horizontal stacked bar chart. X-axis = time (seconds).
Each bar is one backend. Segments colored by step type (worktree, config,
initial build, search, edit, incremental build, git commit, checkpoint).
The search/edit/incremental-build/commit segments are aggregated across
all 5 commits. This shows the total session cost breakdown at a glance.

**CLI**:
```
agfs-bench dev-workflow [--backend <name>]
```

Single command, runs the full workflow on each backend, writes
`dev-workflow.json` and a Plotly HTML report.

#### Worktree (`worktree`)

Runs `git worktree add --detach` from a local Linux kernel clone
(`~/.cache/agfs-bench/linux`). Exercises the read-heavy path: the workload
reads thousands of objects from the base repository and writes a new working
tree into the mount. The fixture (initial clone) is constructed once and
reused; subsequent runs use `git worktree prune` to clean up stale entries
before each `worktree add`.

#### Linux untar (`linux-untar`)

Extracts a cached Linux release tarball into the benchmark destination
directory (`tar -xJf ... --strip-components=1`). This stresses bulk directory
creation, inode allocation, and metadata updates from a large source tree
without requiring a pre-existing git repository in the workload runtime path.

Fixture setup is cached and reused in `~/.cache/agfs-bench/linux-tar/`:

- On first use, the workload downloads one Linux source tarball once.
- Subsequent runs and benchmark invocations reuse the same cached tarball.
- The tarball is mounted read-only for realistic-rule runs while extraction
  output is written inside the backend session work directory.

### Per-op micro-benchmarks

Op benchmarks measure per-syscall throughput and latency inside a mounted
session. The backend mounts once, the workload runs, and results are
self-reported by the subprocess (IOPS, MB/s, latency percentiles). No
init/commit timing is reported — the goal is to isolate the steady-state
overhead of the interposition layer.

#### Big file data operations (fio)

Large-file I/O using [fio](https://github.com/axboe/fio). Each workload
generates a jobfile, runs `fio --output-format=json`, and parses the result.
Buffered I/O (`direct=0`) is used because agfs operates at the VFS level and
real agent workloads use the page cache. The generated fio jobfiles set
`invalidate=0` so fio does not discard the page cache on open. Read-style fio
workloads that need a pre-existing file create the 1 GiB backing file inside
the mounted sandbox before the timed run. Cold variants then have the backend
drop page cache *after* that sandbox-local file is prepared and *before* fio
starts, so the cold transition is controlled by the harness rather than by fio
invalidating an already-open file. Each fio workload uses a 1 GiB backing file
but performs 256 MiB of total I/O so cold random-read runs finish in roughly
10 seconds on the slowest backends while still exercising a large working set.
Seeded fio workloads create that file inside the mounted sandbox with a
deterministic non-zero byte pattern rather than a zero-filled image.

Read workloads come in **cold** and **warm** variants. Cold drops page cache
from the parent process after the sandbox-local backing file has been created
and before fio starts. Warm pre-reads the file so all I/O hits the page cache.
This matters because warm-cache reads isolate pure VFS interposition overhead,
while cold-cache reads include actual disk I/O amplified by interposition.
Writes always go to the page cache regardless, so they have no cold/warm
split.

| Workload | Operation | Cache | What it measures |
|---|---|---|---|
| `fio-seq-read-cold` | Sequential 4K read, 256 MiB over a 1 GiB file | cold | Disk read + agfs lookup overhead |
| `fio-seq-read-warm` | Sequential 4K read, 256 MiB over a 1 GiB file | warm | Pure VFS interposition overhead |
| `fio-seq-write` | Sequential 4K write, 256 MiB over a 1 GiB file | — | Write path + staging overhead |
| `fio-rand-read-cold` | Random 4K read, 256 MiB over a 1 GiB file | cold | Random disk + agfs overhead |
| `fio-rand-read-warm` | Random 4K read, 256 MiB over a 1 GiB file | warm | Random read interposition overhead |
| `fio-rand-write` | Random 4K write, 256 MiB over a 1 GiB file | — | Random write + staging overhead |
| `fio-randrw-cold` | 70/30 read/write mix, 4K, 256 MiB over a 1 GiB file | cold | Mixed I/O, first-access pattern |
| `fio-randrw-warm` | 70/30 read/write mix, 4K, 256 MiB over a 1 GiB file | warm | Mixed I/O, steady-state |

fio must be installed (`apt install fio`). If absent, fio workloads are
skipped with a warning.

#### Small file metadata operations (custom)

Small-file metadata operations, implemented in Rust. Most workloads operate on
10,000 files, record per-operation latency, and compute IOPS and percentiles.
The readdir workloads instead enumerate 1,000 directories with 10 files each
and record one latency sample per directory enumeration.

Read-path metadata ops (stat, readdir) have cold/warm variants for the same
reason as fio reads: cold stat hits disk for inode reads, warm stat is pure
dcache/icache. Write-path ops (create, rename, unlink) are always writes and
have no cold/warm split.

For **cold** metadata variants, a timed iteration should measure a single cold
operation, not an average over a long sequence of accesses. Averaging many
"cold" operations inside one run quickly turns the measurement into a mixed
cold+warm result because dentries, inodes, and small directories become cached
after the first accesses. Repeated benchmark iterations should provide the
sample set for cold metadata latency, while warm metadata variants may still
batch many operations inside one timed run.

Metadata benchmarks also need to distinguish **where the target files live**:

- **base**: the files or directories already exist in the underlying/base
  layer before the backend mount or branch is created
- **stage**: the files or directories are created inside the mounted staging
  view before the timed workload starts
- **checkpoint**: the files are created inside the mounted staging view,
  then a checkpoint is taken before the timed workload starts. The files
  exist in the staging layer but belong to a previous checkpoint, so
  modifications trigger re-COW / copy-up from the checkpointed state.

This distinction matters because operations on base-layer objects exercise
lookup passthrough and copy-up behavior, operations on stage-local objects
exercise the already-staged fast path, and operations on checkpoint-layer
objects exercise the re-COW path where the source is a previously staged
inode rather than the lower filesystem. For metadata-heavy operations such as
`stat`, `readdir`, `append`, `rename`, and `unlink`, the benchmark matrix
should cover `base`, `stage`, and `checkpoint` source variants.

##### Checkpoint mechanics per backend

Each backend implements the checkpoint variant differently:

- **agfs**: The agfs config used by the benchmark sets `checkpoint: false`
  to disable auto-checkpointing. For checkpoint variants, the backend
  explicitly calls `agfs checkpoint` after `prepare_workdir()` and before
  the timed run. This increments `sbi->checkpoint_gen` in the kernel so
  that subsequent opens of the prepared files trigger re-COW via
  `agfs_do_cow`.
- **overlayfs**: The backend unmounts after `prepare_workdir()`, then
  remounts with the old upper directory demoted to an additional lower
  layer and a fresh upper directory. overlayfs supports multiple stacked
  lower directories, so the checkpointed files now live in a lower layer
  and writes trigger copy-up just as they would for base-layer files.
- **branchfs**: After `prepare_workdir()`, the backend creates a nested
  branch (`branchfs create bench2 <mnt>`). Files from the parent branch
  are visible in the nested branch but modifications trigger branchfs's
  copy-on-write.
- **native**: No layering concept. The checkpoint variant runs the same as
  stage — files are ordinary files on disk. This serves as a control to
  confirm that checkpoint overhead is zero without interposition.

The workload code should treat `cache` and `source` as two orthogonal axes:

- `cache`: `cold` or `warm`
- `source`: `base`, `stage`, or `checkpoint`

Each operation should have one shared implementation parameterized by those
axes, with thin workload wrappers providing the concrete CLI name and report
metadata. The shared implementation should split responsibility as follows:

- `populate_base()`: create fixtures only for `source=base`
- `prepare_workdir()`: create fixtures for `source=stage` and
  `source=checkpoint` (both need files inside the mounted view)
- `needs_checkpoint()`: return true only for `source=checkpoint`; the
  backend calls this to decide whether to take a checkpoint between
  `prepare_workdir()` and the timed run
- `cache_mode()`: request page-cache dropping only for `cache=cold`
- `run()`: perform any warm-up pass only for `cache=warm`, then execute the
  timed operation. For `cache=cold`, the timed body should contain exactly
  one cold metadata operation per iteration.

This keeps the benchmark matrix explicit without duplicating the full workload
body for every `cache × source` combination.

| Workload | Operation | Cache | What it measures |
|---|---|---|---|
| `meta-create` | Create 10,000 empty files | — | File creation throughput |
| `meta-append-{base,stage,checkpoint}` | Append 4K to 10,000 files | — | Append + COW throughput on base vs stage vs checkpoint files |
| `meta-open-{cold,warm}-{base,stage,checkpoint}` | Open 10,000 files (`File::open`) | cold / warm | Open-path lookup and inode/dentry resolution overhead across source layers |
| `meta-stat-{cold,warm}-{base,stage,checkpoint}` | Stat 10,000 files | cold / warm | Inode lookup from disk vs cache, across all source layers |
| `meta-readdir-{cold,warm}{,-100}-{base,stage,checkpoint}` | Readdir one directory containing 10,000 or 100 files | cold / warm | Directory enumeration across source layers and directory sizes |
| `meta-rename-{base,stage,checkpoint}` | Rename 10,000 files | — | Rename + journal overhead across all source layers |
| `meta-unlink-{base,stage,checkpoint}` | Unlink 10,000 files | — | Delete + journal overhead across all source layers |

#### Op result model

Op workloads report per-operation metrics instead of wall time:

```
OpResult {
    iops:              f64,          // operations per second
    throughput_kbps:   Option<u64>,  // KB/s (fio workloads only)
    lat_us_p50:        f64,          // median latency in microseconds
    lat_us_p99:        f64,
    lat_us_p999:       f64,
    read_avg_lat_us:   Option<f64>,  // mixed fio only
    write_avg_lat_us:  Option<f64>,  // mixed fio only
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

The branchfs backend performs one untimed `stat` on the workload directory
after mount/branch creation and before the timed workload starts. This absorbs
first-request FUSE/daemon startup overhead without warming file contents or
polluting the page-cache behavior of read benchmarks.

Op benchmarks are rendered as bar charts with backends on the x-axis and
IOPS on the y-axis. Native is shown as a baseline reference line. Bars are
colored by backend.

For workloads with source variants (base/stage/checkpoint), each backend
has grouped bars — one per source variant — distinguished by **fill
pattern**: solid fill for base, diagonal stripes for stage, crosshatch for
checkpoint. The legend shows the three patterns. This keeps the backend
color encoding intact while adding the source dimension without extra
charts. Workloads without source variants (e.g. `meta-create`, fio
workloads) use plain solid bars as before.

Cold and warm cache variants are rendered as **separate charts** because
they measure fundamentally different things (disk I/O vs cache hits) and
have different y-axis scales. The charts are placed adjacent to each other
in the report grid for easy visual comparison.

For mixed fio workloads, the chart shows separate read and write average
latencies per backend using filled vs outlined bars. Latency percentiles
(p50/p99) are shown in a table below each chart, along with average latency
and throughput for fio workloads.
If one backend is an extreme outlier relative to native, the report keeps a
single chart but visually caps that bar, gives it a distinct appearance, and
labels it with how many times larger it is than the native baseline. This
keeps the normal backends readable without hiding the outlier.

The report index page groups results into three sections: Session Micro,
Session Macro, and Per-Operation. Op benchmarks are included in the default
(no-flags) run alongside session benchmarks.

For long-running sweeps, `agfs-bench --skip-complete --runs N` resumes from
the current `results.json`: any `(workload, backend)` pair that already has
exactly `N` timed iterations recorded is skipped, while missing or partially
recorded pairs are still executed and merged back into the report.

`agfs-bench` must be run from a release build. The binary exits immediately
when compiled with debug assertions so benchmark numbers do not come from an
unoptimized runner.

Each recorded backend result also stores the repository commit plus whether
`user/` and `kmod/` were dirty at run time. The HTML report compares those
recorded states against the current checkout and marks each workload as fresh
or stale. A plain `HEAD` change does not make a result stale by itself: the
report asks git whether `user/` or `kmod/` actually differ between the recorded
commit and the current one, and also tracks `user/` / `kmod/` dirty/clean
transitions.

---

## 3. Backends

Each workload is run under multiple backends. A backend defines how writes are
staged and committed. The goal is to isolate the cost of each mechanism and
place agfs in context relative to alternatives.

| Backend | Mechanism | Needs root? | Default? |
|---|---|---|---|
| `native` | Direct ext4 writes, no staging | no | yes |
| `agfs-no-perm` | Kernel stackable fs; permission gating disabled (`permission=false`) | no (setuid) | yes |
| `agfs-realistic` | Kernel stackable fs; workload-defined rules | no (setuid) | yes |
| `overlayfs` | User-namespace overlayfs; replay upper on commit | no (user-ns) | yes |
| `branchfs` | FUSE copy-on-write branches; `branchfs commit` | no | yes |

`agfs-bench` does **not** need to run as root. The agfs binary is setuid,
overlayfs uses user namespaces, and branchfs runs in userspace. Only
the profiler (§7) invokes `sudo` internally for `perf` and `bpftrace`.

### agfs backends

The agfs backend is split into two configurations to isolate the cost of each
level of gating:

| Backend | Configuration | What it measures |
|---|---|---|
| `agfs-no-perm` | `permission=false` | VFS interposition + staging only; permission checks are fully disabled |
| `agfs-realistic` | `permission=true`, workload-defined rules | Typical rule-based config with permission checking enabled |

`agfs-no-perm` is the lower bound for pure agfs interposition overhead.
`native` is the absolute floor.

Both agfs backend configurations set `checkpoint: false` in the generated
`agfs.toml` to prevent `agfs exec` from auto-checkpointing. For workloads
that need a checkpoint (the `source=checkpoint` metadata variants), the
backend explicitly calls `agfs checkpoint` between `prepare_workdir()` and
the timed run, giving the harness full control over when `checkpoint_gen`
is incremented.

### overlayfs

The `overlayfs` backend uses Linux overlayfs directly, without any wrapper
tool. Each iteration:

1. Creates a fresh tempdir with `lower/`, `upper-0/`, `work-0/`, `merged/`.
2. Mounts overlayfs via `sudo mount -t overlay`.
3. Runs the workload inside the merged directory.
4. Commits by replaying the upper dir onto lower: regular files are renamed
   (O(1) on the same filesystem), whiteout devices (char 0,0) trigger
   deletions, opaque directories (`user.overlay.opaque` xattr) replace their
   lower counterparts, and symlinks are recreated.

For checkpoint workloads, the backend does not flatten staged changes into
base at checkpoint creation. Instead it remounts with multiple lower layers:
the current upper is turned into an additional read-only lower layer and a
fresh upper/work pair is mounted on top. Final commit flattens layers in
creation order (oldest checkpoint layer first, newest layer last) so later
layers correctly override earlier ones.

For `checkpoint-scalability`, overlayfs runs in best-effort mode: if remounting
with additional lower layers fails (for example due to mount option/lowerdir
limits), the workload stops adding points and emits the partial series
collected so far.

This gives a clean measurement of overlayfs overhead without shell noise.

### branchfs

`branchfs` is a FUSE filesystem (from `bench/third_party/branchfs`) that provides
O(1) branch creation and atomic commit-to-parent semantics. Each iteration:

1. Mounts branchfs over a fresh base directory with a per-iteration storage
   directory (`branchfs mount --base <base> --storage <storage> <mnt>`).
2. Creates a `bench` branch (`branchfs create bench <mnt>`).
3. Runs the workload inside the mount (via `exec-workload` subprocess).
4. Commits the branch (`branchfs commit <mnt>`).
5. Unmounts (`branchfs unmount <mnt>`).

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
| `branchfs` | `branchfs mount` + `create` | workload | `branchfs commit` |

Every backend runs the workload as a subprocess via the `exec-workload`
subcommand. The subprocess prints a `READY` marker to stdout just before it
starts the workload. The parent watches for this marker — wall time before it
arrives is startup overhead (process spawn, or for `overlayfs`, full
namespace + overlayfs setup), wall time after is staging. For backends with a
separate init step (agfs, branchfs), init is measured in the parent before
spawning the subprocess; for `overlayfs`, init *is* the startup time
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
    agfs.rs        — agfs-no-perm + agfs-realistic + ProfileSession
    overlayfs.rs   — direct overlayfs in user namespace
    branchfs.rs
  workload.rs      — Workload trait + IterResult
  workloads/
    mod.rs         — registry + shared helpers for op workloads
    <name>.rs      — one file per workload
  profiler.rs      — bpftrace + perf flamegraph
  report.rs        — plotly HTML report
```

Op workloads share helper code in `workloads/mod.rs` for:

- emitting the `AGFS_BENCH_RESULTS` JSON line,
- computing latency percentiles from per-operation samples,
- dropping or warming caches for cold/warm read-path benchmarks,
- generating fio jobfiles from a small set of parameters.

The individual workload files stay thin wrappers around those helpers so the
benchmark matrix can grow without duplicating the same measurement code in
every file. Workload-specific report metadata (summary, fixture notes, exact
fio spec display, etc.) lives alongside the workload implementation in each
`workloads/<name>.rs` file rather than in one central registry blob.

### Backend availability and visibility

Each backend implements `available()`, `unavailable_reason()`, and `hidden()`.

- **Unavailable** backends are missing required tools; they are always skipped.
- **Hidden** backends are functional but excluded from default runs because
  they add noise. Use `--backend <name>` to run them explicitly.

`agfs-bench list` shows all backends with their status.

### Third-party tools

| Tool | Source | Install |
|---|---|---|
| `branchfs` | `bench/third_party/branchfs/` | `make -C bench install-branchfs` |

### CLI

```
agfs-bench [--workload <name> ...] [--backend <name>] [--micro] [--macro] [--op]
           [--op-group <meta|fio>]
           [--runs N] [--verbose] [--timestamped-results]
agfs-bench rerender
agfs-bench list
agfs-bench profile [--workload <name>] [--scenario <name>] [--no-bpftrace]
agfs-bench exec-workload --name <name> --dest <path> [--verbose]
```

- With no flags: runs all workloads × all available non-hidden backends.
- `--micro` / `--macro` / `--op`: run only session micro, session macro, or
  per-operation benchmarks respectively.
- `--op-group <meta|fio>`: with `--op`, further narrow the per-operation run
  to metadata workloads or fio workloads only.
- `--workload` / `--backend`: filter to a specific combination. `--workload`
  may be repeated to run multiple named workloads. `--backend` overrides
  hidden status for any backend.
- For source-variant metadata workloads, `--workload` accepts either an
  individual variant (for example `meta-append-stage`) or the group name
  (for example `meta-append`), which expands to
  `{base,stage,checkpoint}` variants.
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
  Backends run the subprocess with `cwd=--dest` and workload file operations
  use relative paths rooted at `.`.

### Logging and failure handling

On failure, the failing (workload, backend) combination is automatically rerun
with verbose logging enabled. Verbose logs include:

- Workload stdout/stderr
- agfs audit contents at the point of failure (agfs backend only)

### Results

Results are written to `results-bench/<hostname>/`. By default the previous
result for that host is overwritten; pass `--timestamped-results` to retain
multiple runs.

Each result records environment metadata (CPU, memory, storage device and model,
filesystem type, kernel version, distro) so results from different machines are
not conflated. Running `--workload X` or `--backend Y` merges only the
re-run entries into the existing `results.json`, preserving results for
workloads and backends that were not part of the current run.

Persistence is incremental: after each completed `(workload, backend)`
combination, the bench runner rewrites `results.json` immediately. Report
generation is incremental too: it rerenders only `report-<workload>.html` for
the workload that changed, plus the index page, instead of rebuilding every
workload report on every update.

An HTML report (`report-<workload>.html`) is generated per workload using the
[`plotly`](https://crates.io/crates/plotly) crate:

- **Session workloads**: stacked bar charts showing backend × (init, staging,
  commit) time. Native shown only as a reference line/annotation, with the
  backend bars showing the non-native mechanisms under comparison. Error bars
  show total stddev across iterations.
- **Op workloads**: bar charts with backends on the x-axis and IOPS on the
  y-axis. For fio workloads, throughput (MB/s) on a secondary axis. Latency
  percentiles (p50/p99) in a table below each chart. Native shown only as a
  baseline line/annotation, not as a bar.

The index page groups results into three sections: Session Micro, Session
Macro, and Per-Operation. Each workload card/report also includes a compact
workload explainer: a hover summary on the title plus an expandable details
panel describing fixture setup, cache behavior, and the concrete fio or Rust
operation sequence being benchmarked. For fio workloads, the details panel
shows the exact fio command form and the generated jobfile text used by the
bench runner.

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
| `agfs_journal_stage` | Journal S record for staged entry (create/mkdir/symlink/COW) |
| `agfs_journal_delete` | Journal D record for deletion |
| `agfs_journal_rename` | Journal R record for rename |
| `agfs_journal_mark` | Journal M record for checkpoint |
| `agfs_release` | File release |
| `agfs_fill_base` | Readdir phase-2 base-entry dedup |

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
Profile: write-files / agfs-no-perm  (wall: 167 ms)

  op                               calls  median µs  p99 µs    total ms
  --------------------------------------------------------------------------
  create                            1000         16    1024       100.5
  create_staged                     1000         16    1024       100.1
  staging_alloc                     1000         16      64        27.6
  lookup                            1000          8      32        15.2
  write_iter                        1000          4      32         7.4
  open                              1000          2       8         2.5
  journal_stage                     1000          1       8         1.6
```

The `total ms` column ranks optimization targets by contribution to wall time.

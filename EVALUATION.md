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

The benchmark suite serves two purposes:

- **Artifact evaluation**: comprehensive, reproducible results demonstrating
  agfs overhead across realistic workloads and permission configurations.
- **Iterative development**: a fast subset that gives quick feedback when
  optimising the kernel module or userspace tooling.

---

## 2. Workloads

The suite defines multiple workloads to avoid overfitting to a single access
pattern. Each workload is a self-contained Rust function that performs a
specific operation on the mounted filesystem.

### Clone (`clone-large`)

Full `git clone --no-local --no-hardlinks` of the Linux kernel source from a
local bare mirror (`~/.cache/agfs-bench/linux.git`, fetched automatically on
first run). Tests lookup, create, and readdir at scale (~1.2M files).

Primarily stresses write and metadata paths.

---

## 3. Permission Scenarios

Each workload is run under each of the following permission configurations.
The goal is to isolate the cost of each level of gating.

| Scenario | Configuration | What it measures |
|---|---|---|
| `native` | No agfs, direct ext4 | True baseline |
| `rules-allow-all` | `allow-rw /` rule | VFS interposition + staging; no per-access gating |
| `ask-default-allow` | `ask_default=allow` | Fast-path gating resolved without IPC |
| `rules-realistic` | workload-defined rules | Typical rule-based config; most accesses hit cache |
| `ask-daemon-allow` | default (`ask`) + auto-approve daemon | Full IPC round-trip on every ungated access |

`rules-allow-all` is the practical floor for a useful agfs configuration: all
writes are staged, all reads pass through VFS interposition, but no gating
overhead is paid per-access. `native` is the absolute floor.

The auto-approve daemon used by `ask-daemon-allow` is an internal
implementation detail of the bench binary — it is spawned and managed
automatically as part of fixture setup, not exposed as a user-facing command.

---

## 4. Timing Model

For write workloads, time is decomposed into three phases and reported
separately:

```
total = staging_time + commit_time
```

- **`staging_time`**: wall time of the workload itself (clone, edit, etc.)
  while running inside agfs. This is what the agent experiences.
- **`commit_time`**: wall time of `agfs commit` after the workload completes.
  This is the cost of flushing staged changes to the base filesystem.
- **`abort_time`**: wall time of `agfs abort` (measured in `staging-lifecycle`).
  Should be near-instantaneous regardless of staging size.

For read-only workloads (`find-files`, `grep-tree`), staging is not involved
and only a single wall-time figure is reported.

All timings are taken with `std::time::Instant` inside the bench binary.
Each (workload, scenario, cache-state) triple is run 10 times (3 in quick
mode); mean ± stddev are reported, and outliers (>2σ) are flagged.

---

## 5. Fixture vs Run

**Fixture** (setup, not timed): constructed once and reused across all
subsequent runs. If the fixture already exists it is not rebuilt.

- Mirror repository fetched to `~/.cache/agfs-bench/linux.git` if not present.
- agfs mounted with the appropriate options and rules.
- For `ask-daemon-allow`: the auto-approve daemon spawned as a child process
  and confirmed listening before timing begins.

**Warm-up**: the workload is run once in full (including commit) before timing
begins. This populates the page cache for the mirror and warms the dentry/inode
caches on the agfs mount. The warm-up result is discarded.

**Run** (timed): the workload is run N+1 times total (1 warm-up + N timed).
Mean ± stddev of the N timed iterations is reported; outliers (>2σ) are
flagged.

**Teardown**: performed automatically on exit — agfs unmounted, daemon stopped,
temporary directories removed.

---

## 6. Implementation

The benchmark suite is a Rust binary (`agfs-bench`) in the same Cargo
workspace as the CLI, under `bench/src/main.rs`. It shares ioctl types, mount
helpers, and config parsing with the CLI via the existing library crate.

The only user-facing interface is:

```
agfs-bench [--workload <name>] [--scenario <name>] [--verbose]
agfs-bench rerender
```

- With no flags: runs all workloads × all scenarios.
- `--workload` / `--scenario`: filter to a single combination.
- `--verbose`: capture detailed logs for all runs even on success.
- `rerender`: regenerate the HTML report from the existing results JSON without
  re-running any benchmarks. Useful when iterating on the visualisation.

### Logging and failure handling

Each run captures basic timing and workload output. On failure, the failing
(workload, scenario) combination is automatically rerun with verbose logging
enabled and the logs saved alongside the results for post-mortem inspection.

Verbose logs include:
- Workload stdout/stderr
- Kernel messages produced during the run, captured via the systemd journal
  using the same cursor-snapshot approach as the integration test helpers
  (`snapshot_journal` before the run, `kernel_messages_since` after)
- agfs journal contents at the point of failure

The systemd journal utilities in `tests/helpers.rs` should be extracted into
the shared library crate so the bench binary can reuse them directly.

Results are written to `results/<hostname>/` in the repository. By default the
previous result for that host is overwritten and a new commit made; git history
preserves all prior runs. Pass `--timestamped-results` to write into a timestamped
subdirectory (`results/<hostname>/<timestamp>/`) instead, retaining multiple
results from the same machine in the working tree.

Each result records environment metadata in the JSON so results from different
machines are not conflated: hardware (CPU model, memory size, storage type) and
software (kernel version, Linux distribution).

Setup (fetching the linux mirror, creating directories) is performed
automatically before the first workload that requires it.

Results are written to JSON and an HTML report is generated using the
[`plotly`](https://crates.io/crates/plotly) crate. The report contains:

- Grouped bar charts: workload × scenario, showing staging time and commit
  time as stacked bars, with native as a reference line.
- Error bars showing stddev across iterations.
- Separate panels for warm-cache and cold-cache read workloads.

`make bench` runs `agfs-bench`. It is not part of the default CI pipeline; it
is triggered manually via `workflow_dispatch` on a dedicated GitHub Actions
workflow, or run locally after `make load-kmod`.


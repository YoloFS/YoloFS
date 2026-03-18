#!/usr/bin/env bash
# install_deps.sh — Install additional dependencies for agfs-bench.

set -euo pipefail

info() { echo -e "\033[1;34m==>\033[0m $*"; }

BENCH_PKGS=(
    # per-operation I/O benchmarks
    fio                        # flexible I/O tester for throughput/latency measurement
    # profiling
    "linux-tools-$(uname -r)"  # perf for flamegraph generation
    bpftrace                   # per-op latency histograms on agfs kfuncs
    # try backend build deps (third_party/try)
    autoconf                   # try's autotools build system
    attr                       # xattr utilities used by try
    pandoc                     # try builds its man page
    # branchfs backend build deps (third_party/branchfs)
    fuse3                      # FUSE runtime for branchfs
    libfuse3-dev               # FUSE headers for branchfs compilation
    # btrfs backend
    btrfs-progs                # mkfs.btrfs, btrfs subvolume commands
    rsync                      # btrfs commit copies changes back to base
)

info "Installing benchmark dependencies"
sudo apt-get update -qq
sudo apt-get install -y --no-install-recommends "${BENCH_PKGS[@]}"

# ── Kernel settings required by try ──────────────────────────────────

info "Configuring kernel settings for try (overlay + unprivileged userns)"
sudo modprobe overlay
sudo sysctl -w kernel.apparmor_restrict_unprivileged_userns=0

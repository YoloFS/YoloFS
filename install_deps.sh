#!/usr/bin/env bash
# install_deps.sh — Install dependencies for agfs.

set -euo pipefail

info() { echo -e "\033[1;34m==>\033[0m $*"; }

is_vm() { systemd-detect-virt -q 2>/dev/null; }

# ── Package lists ─────────────────────────────────────────────────────

QEMU_PKGS=(
    qemu-system-x86
    qemu-utils
    cloud-image-utils
    wget
)

BUILD_PKGS=(
    build-essential
    "linux-headers-$(uname -r)"
    "linux-tools-$(uname -r)"
    bc
    kmod
    pkg-config
    libsystemd-dev
    git
    pahole
    bpftrace
    # try build deps
    autoconf
    attr
    pandoc
    # branchfs build deps
    fuse3
    libfuse3-dev
    # btrfs backend
    btrfs-progs
    rsync
)

# ── Installers ────────────────────────────────────────────────────────

install_apt() {
    local pkgs=("$@")
    sudo apt-get update -qq
    sudo apt-get install -y --no-install-recommends "${pkgs[@]}"
}

install_rust() {
    if command -v rustc &>/dev/null; then
        info "Rust already installed: $(rustc --version)"
    else
        info "Installing Rust via rustup"
        curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y -q
        . "$HOME/.cargo/env"
    fi
}

# ── Main ─────────────────────────────────────────────────────────────

pkgs=("${BUILD_PKGS[@]}")

if is_vm; then
    info "Detected VM environment, skipping QEMU packages"
else
    info "Detected host environment, including QEMU packages"
    pkgs+=("${QEMU_PKGS[@]}")
fi

install_apt "${pkgs[@]}"
install_rust

# ── Kernel settings required by try ──────────────────────────────────

info "Configuring kernel settings for try (overlay + unprivileged userns)"
sudo modprobe overlay
sudo sysctl -w kernel.apparmor_restrict_unprivileged_userns=0

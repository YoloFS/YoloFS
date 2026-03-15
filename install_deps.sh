#!/usr/bin/env bash
# install_deps.sh — Install dependencies for agfs.
#
# Usage:
#   ./install_deps.sh host  — Host dependencies (QEMU only)
#   ./install_deps.sh vm    — VM dependencies (build tools, Rust, etc.)

set -euo pipefail

info() { echo -e "\033[1;34m==>\033[0m $*"; }

# ── Package lists ─────────────────────────────────────────────────────

HOST_PKGS=(
    qemu-system-x86
    qemu-utils
    cloud-image-utils
    wget
)

VM_PKGS=(
    build-essential
    "linux-headers-$(uname -r)"
    bc
    kmod
    pkg-config
    libsystemd-dev
    git
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
        curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
        # shellcheck source=/dev/null
        source "$HOME/.cargo/env"
    fi
}

# ── Subcommands ───────────────────────────────────────────────────────

install_host() {
    info "Installing host dependencies"
    install_apt "${HOST_PKGS[@]}"
}

install_vm() {
    info "Installing VM guest dependencies"
    install_apt "${VM_PKGS[@]}"
    install_rust
}

usage() {
    echo "Usage: $0 {host|vm}"
    echo ""
    echo "  host  Host dependencies (QEMU only)"
    echo "  vm    VM dependencies (build tools, Rust, etc.)"
    exit 1
}

case "${1:-}" in
    host) install_host ;;
    vm)   install_vm ;;
    *)    usage ;;
esac

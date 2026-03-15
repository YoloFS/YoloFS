#!/usr/bin/env bash
# install_deps.sh — Install dependencies for agfs.
#
# Usage:
#   ./install_deps.sh dev   — Host dev dependencies (build tools, Rust, QEMU, etc.)
#   ./install_deps.sh vm    — In-VM dependencies for building/running agfs

set -euo pipefail

info() { echo -e "\033[1;34m==>\033[0m $*"; }

# ── Package lists ─────────────────────────────────────────────────────

COMMON_PKGS=(
    build-essential
    "linux-headers-$(uname -r)"
    bc
    kmod
    pkg-config
    libsystemd-dev
)

DEV_PKGS=(
    qemu-system-x86
    qemu-utils
    cloud-image-utils
    wget
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

install_uv() {
    if [[ "${CI:-}" == "true" ]]; then
        info "Skipping uv install in CI"
    elif command -v uv &>/dev/null; then
        info "uv already installed: $(uv --version)"
    else
        info "Installing uv"
        curl -LsSf https://astral.sh/uv/install.sh | sh
    fi
}

print_summary() {
    echo ""
    info "Dependencies installed:"
    echo "  kernel headers: $(dpkg -s "linux-headers-$(uname -r)" 2>/dev/null | grep Version | cut -d' ' -f2)"
    echo "  gcc:            $(gcc --version | head -1)"
    echo "  rustc:          $(rustc --version 2>/dev/null || echo 'not found')"
    echo "  cargo:          $(cargo --version 2>/dev/null || echo 'not found')"
    echo "  uv:             $(uv --version 2>/dev/null || echo 'not found')"
}

# ── Subcommands ───────────────────────────────────────────────────────

install_dev() {
    info "Installing host dev dependencies"
    install_apt "${COMMON_PKGS[@]}" "${DEV_PKGS[@]}"
    install_rust
    install_uv
    print_summary
}

install_vm() {
    info "Installing VM guest dependencies"
    install_apt "${COMMON_PKGS[@]}"
    install_rust
    install_uv
    print_summary
}

usage() {
    echo "Usage: $0 {dev|vm}"
    echo ""
    echo "  dev   Host dev dependencies (build tools, Rust, QEMU, etc.)"
    echo "  vm    In-VM dependencies for building/running agfs"
    exit 1
}

case "${1:-}" in
    dev) install_dev ;;
    vm)  install_vm ;;
    *)   usage ;;
esac

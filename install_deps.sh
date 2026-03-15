#!/usr/bin/env bash
# install_deps.sh — Install all dependencies for building agfs.
#
# Usage: ./install_deps.sh
#
# Installs:
#   - Linux kernel headers + build tools (for kmod/)
#   - Rust toolchain (for cli/ source)
#   - uv (Python package manager, for scripts/)

set -euo pipefail

info() { echo -e "\033[1;34m==>\033[0m $*"; }

# ── Linux kernel headers & build tools ────────────────────────────────

info "Installing kernel headers and build tools"
sudo apt-get update -qq
sudo apt-get install -y --no-install-recommends \
    build-essential \
    linux-headers-"$(uname -r)" \
    bc \
    kmod \
    pkg-config \
    libsystemd-dev

# ── Rust ──────────────────────────────────────────────────────────────

if command -v rustc &>/dev/null; then
    info "Rust already installed: $(rustc --version)"
else
    info "Installing Rust via rustup"
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
    # shellcheck source=/dev/null
    source "$HOME/.cargo/env"
fi

# ── uv (Python package manager) ──────────────────────────────────────

if [[ "${CI:-}" == "true" ]]; then
    info "Skipping uv install in CI"
elif command -v uv &>/dev/null; then
    info "uv already installed: $(uv --version)"
else
    info "Installing uv"
    curl -LsSf https://astral.sh/uv/install.sh | sh
fi

# ── Summary ───────────────────────────────────────────────────────────

echo ""
info "All dependencies installed:"
echo "  kernel headers: $(dpkg -s "linux-headers-$(uname -r)" 2>/dev/null | grep Version | cut -d' ' -f2)"
echo "  gcc:            $(gcc --version | head -1)"
echo "  rustc:          $(rustc --version)"
echo "  cargo:          $(cargo --version)"
echo "  uv:             $(uv --version)"

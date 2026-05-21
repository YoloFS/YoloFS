#!/usr/bin/env bash
# setup.sh — Install dependencies for YoloFS.

set -euo pipefail

info() { echo -e "\033[1;34m==>\033[0m $*"; }

is_vm() { systemd-detect-virt -q 2>/dev/null; }

# ── Package lists ─────────────────────────────────────────────────────

QEMU_PKGS=(
    qemu-system-x86   # VM emulation (vm.py)
    qemu-utils        # qemu-img for disk image management
    cloud-image-utils  # cloud-localds for VM cloud-init
)

BUILD_PKGS=(
    build-essential                # gcc, make — kernel module compilation
    "linux-headers-$(uname -r)"   # kernel headers for kmod build
    bc                             # kernel build arithmetic
    kmod                           # insmod/rmmod for loading yolofs.ko
)

# ── APT packages ──────────────────────────────────────────────────────

pkgs=("${BUILD_PKGS[@]}")

if is_vm; then
    info "Detected VM environment, skipping QEMU packages"
else
    info "Detected host environment, including QEMU packages"
    pkgs+=("${QEMU_PKGS[@]}")
fi

missing=()
for pkg in "${pkgs[@]}"; do
    if ! dpkg-query -W -f='${Status}' "$pkg" 2>/dev/null | grep -q 'install ok installed'; then
        missing+=("$pkg")
    fi
done

if (( ${#missing[@]} > 0 )); then
    info "Installing missing packages: ${missing[*]}"
    sudo apt-get update -qq
    sudo apt-get install -y --no-install-recommends "${missing[@]}"
else
    info "All APT packages already installed: ${pkgs[*]}"
fi

# ── Rust toolchain (host only — cargo is not invoked inside the VM) ──

if ! is_vm; then
    if command -v rustc &>/dev/null; then
        info "Rust already installed: $(rustc --version)"
    else
        info "Installing Rust via rustup"
        curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y -q
        . "$HOME/.cargo/env"
    fi
fi

# ── /dev/kmsg access ──────────────────────────────────────────────────
# Tests and benchmarks read /dev/kmsg to capture kernel messages.
# By default dmesg_restrict=1 blocks non-root reads.

if [ "$(cat /proc/sys/kernel/dmesg_restrict 2>/dev/null)" != "0" ]; then
    info "Setting kernel.dmesg_restrict=0 for /dev/kmsg access"
    sudo sysctl -w kernel.dmesg_restrict=0 >/dev/null
    echo 'kernel.dmesg_restrict=0' | sudo tee /etc/sysctl.d/99-yolofs.conf >/dev/null
fi

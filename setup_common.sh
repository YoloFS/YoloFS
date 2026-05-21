#!/usr/bin/env bash
# Common deps for any machine that builds/tests yolofs — host, VM, or
# CI. The host-specific QEMU bits and VM bootstrap live in setup.sh.

set -euo pipefail

PKGS=(
    build-essential               # gcc, make — cargo linker, kmod compile
    "linux-headers-$(uname -r)"   # kernel headers for kmod build
    bc                            # kernel build arithmetic
    kmod                          # insmod/rmmod
)

sudo apt-get update
sudo apt-get install -y --no-install-recommends "${PKGS[@]}"

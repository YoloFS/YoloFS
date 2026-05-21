#!/usr/bin/env bash
# One-shot setup for the filesystem repo on a dev host: installs common
# deps, QEMU bits to launch the VM, then boots the VM and installs
# common deps inside it. CI invokes setup_common.sh directly (no
# host-side QEMU needed). The Rust toolchain is installed by the
# top-level YoloFS/setup.sh.

set -euo pipefail

# Host: common build/test deps
./setup_common.sh

# Host-only: QEMU bits to launch the VM (apt index was just refreshed
# by setup_common.sh).
sudo apt-get install -y --no-install-recommends \
    qemu-system-x86 qemu-utils cloud-image-utils

# VM: common build/test deps inside the guest. ./vm.py auto-downloads
# the image and boots the VM on first run.
./vm.py -- ./setup_common.sh

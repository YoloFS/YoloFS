#!/usr/bin/env bash
set -euxo pipefail

deps=(build-essential linux-headers-$(uname -r) bc kmod libcap2-bin clangd bear)
vm_deps=(qemu-system-x86 qemu-utils xorriso)

if [[ "$(hostname)" != "ubuntu-vm" ]]; then
    deps+=("${vm_deps[@]}")
fi

sudo apt-get update
sudo apt-get install -y --no-install-recommends "${deps[@]}"

#!/usr/bin/env bash
set -euxo pipefail

build_deps=(build-essential 'linux-headers-$(uname -r)' bc kmod)
dev_deps=(qemu-system-x86 qemu-utils cloud-image-utils clangd bear)

install='sudo apt-get update && sudo apt-get install -y --no-install-recommends'

# Host: build deps + dev tooling ($(uname -r) resolves here).
eval "$install ${build_deps[*]} ${dev_deps[*]}"

# VM: build deps only ($(uname -r) resolves inside the VM).
./vm.py -- "$install ${build_deps[*]}"

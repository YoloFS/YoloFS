#!/usr/bin/env bash
# Set up the filesystem repo on a dev host.

set -euxo pipefail

./setup_deps.sh

sudo apt-get install -y --no-install-recommends \
    qemu-system-x86 qemu-utils cloud-image-utils clangd bear

./vm.py -- ./setup_deps.sh

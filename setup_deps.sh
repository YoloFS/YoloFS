#!/usr/bin/env bash
# Install build/test deps for the host or VM.

set -euxo pipefail

sudo apt-get update
sudo apt-get install -y --no-install-recommends \
    build-essential \
    "linux-headers-$(uname -r)" \
    bc \
    kmod

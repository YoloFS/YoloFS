#!/usr/bin/env bash
set -euxo pipefail

deps=(build-essential linux-headers-$(uname -r) bc kmod libcap2-bin clangd bear)
if ! dpkg -s "${deps[@]}" &>/dev/null; then
    sudo apt-get update
    sudo apt-get install -y --no-install-recommends "${deps[@]}"
fi

if ! command -v rustc &>/dev/null; then
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y -q
    . "$HOME/.cargo/env"
fi

if ! command -v uv &>/dev/null; then
    curl -LsSf https://astral.sh/uv/install.sh | sh
fi

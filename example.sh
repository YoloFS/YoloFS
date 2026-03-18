#!/bin/bash
# AgFS CLI walkthrough — stages, checkpoints, and restore.
set -euo pipefail

section() { echo; echo "════════════════════════════════════════════════════════════"; echo "  $1"; echo "════════════════════════════════════════════════════════════"; echo; }
run()     { echo "\$ $*"; "$@"; echo; }

# ─── Setup ──────────────────────────────────────────────────────────

section "Setup: load kernel module, init config, mount filesystem"
run agfs load
run agfs init
run agfs mount

# ─── Stage a change, inspect it, then discard ──────────────────────

section "Stage a change, inspect it, then discard"
run agfs exec -- sh -c 'echo hello > greeting.txt'
run agfs status
run agfs diff
run agfs abort --force

# ─── Build up checkpoints ──────────────────────────────────────────

section "Build up checkpoints (each exec auto-creates one)"
run agfs exec -- sh -c 'echo step1 > step1.txt'
run agfs exec -- sh -c 'echo step2 > step2.txt'
run agfs exec -- sh -c 'echo step3 > step3.txt'
run agfs log

# ─── Query across checkpoints ──────────────────────────────────────

section "Query across checkpoints"
run agfs status --at 3
run agfs diff   --from 2 --to 4

# ─── Restore to an earlier checkpoint ──────────────────────────────

section "Restore to checkpoint 2"
run agfs exec -- sh -c 'ls step*.txt'
run agfs restore 2
run agfs exec -- sh -c 'ls step*.txt'
run agfs log

# ─── Teardown ───────────────────────────────────────────────────────

section "Teardown"
run agfs abort --force
run agfs unmount
run agfs unload

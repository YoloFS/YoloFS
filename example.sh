#!/bin/bash
# AgFS CLI walkthrough — stages, checkpoints, and restore.
set -euo pipefail

section() { echo; echo "════════════════════════════════════════════════════════════"; echo "  $1"; echo "════════════════════════════════════════════════════════════"; echo; }
run()     { echo "\$ $*"; "$@"; echo; }

# ─── Setup ──────────────────────────────────────────────────────────

section "Setup: load kernel module, init config, mount filesystem"
run agfs reload
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
run agfs audit

# ─── Query across checkpoints ──────────────────────────────────────

section "Query across checkpoints"
run agfs status --at 2
run agfs diff   --from 1 --to 3

# ─── Restore to an earlier checkpoint ──────────────────────────────

section "Restore to checkpoint 1"
run agfs exec -- sh -c 'ls step*.txt'
run agfs restore 1
run agfs exec -- sh -c 'ls step*.txt'
run agfs exec -- sh -c 'echo step2_new > step2_new.txt'
run agfs audit

# ─── Teardown ───────────────────────────────────────────────────────

section "Teardown"
run agfs abort --force
run agfs unmount
run agfs unload

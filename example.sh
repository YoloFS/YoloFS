#!/bin/bash
# YoloFS CLI walkthrough — stages, snapshots, and travel.
set -euo pipefail

section() { echo; echo "════════════════════════════════════════════════════════════"; echo "  $1"; echo "════════════════════════════════════════════════════════════"; echo; }
run()     { echo "\$ $*"; "$@"; echo; }

# ─── Setup ──────────────────────────────────────────────────────────

section "Setup: load kernel module, init config, mount filesystem"
run yolo reload
run yolo init
run yolo mount

# ─── Stage a change, inspect it, then discard ──────────────────────

section "Stage a change, inspect it, then discard"
run yolo exec -- sh -c 'echo hello > greeting.txt'
run yolo status
run yolo diff
run yolo abort --force

# ─── Build up snapshots ──────────────────────────────────────────

section "Build up snapshots (each exec auto-creates one)"
run yolo exec -- sh -c 'echo step1 > step1.txt'
run yolo exec -- sh -c 'echo step2 > step2.txt'
run yolo exec -- sh -c 'echo step3 > step3.txt'
run yolo audit

# ─── Query across snapshots ──────────────────────────────────────

section "Query across snapshots"
run yolo status --at 2
run yolo diff   --from 1 --to 3

# ─── Travel to an earlier snapshot ──────────────────────────────

section "Travel to snapshot 1"
run yolo exec -- sh -c 'ls step*.txt'
run yolo travel 1
run yolo exec -- sh -c 'ls step*.txt'
run yolo exec -- sh -c 'echo step2_new > step2_new.txt'
run yolo audit

# ─── Teardown ───────────────────────────────────────────────────────

section "Teardown"
run yolo abort --force
run yolo unmount
run yolo unload

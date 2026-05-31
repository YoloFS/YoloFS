#!/bin/bash
# YoloFS CLI walkthrough — stages, snapshots, travel, and permission rules.
# Runs inside example/ so it uses that directory's yolofs.toml.
set -euo pipefail

cd "$(dirname "$0")/example"

section() { echo; echo "════════════════════════════════════════════════════════════"; echo "  $1"; echo "════════════════════════════════════════════════════════════"; echo; }
# Echo the command (quoting args with spaces so it reads as typed), then run it.
run() {
  local shown="" a
  for a in "$@"; do
    case "$a" in *" "*) shown+=" '$a'" ;; *) shown+=" $a" ;; esac
  done
  echo "\$${shown}"; "$@"; echo
}

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

# ─── Permission rules: blocked + ask are recorded in the audit ──────

section "Permission rules: blocked and ask accesses are recorded"
# Two files to restrict (rules can only target paths that exist).
echo "secret"  > secret.txt
echo "k = v"   > config.ini
run yolo rule deny secret.txt
run yolo rule ask  config.ini
# A denied read and an ask (no daemon → resolved by ask_default = deny) both
# fail with EACCES but leave observational notes in the journal; '|| true'
# keeps the walkthrough going.
run yolo exec -- sh -c 'cat secret.txt' || true
run yolo exec -- sh -c 'cat config.ini' || true
run yolo audit

# ─── Teardown ───────────────────────────────────────────────────────

section "Teardown"
run yolo abort --force
run yolo unmount
run yolo unload
# Undo the demo's edits so example/ stays pristine: drop the rules it added
# (toml_edit makes set+unset byte-neutral) and remove the scratch files.
yolo rule unset secret.txt >/dev/null 2>&1
yolo rule unset config.ini >/dev/null 2>&1
rm -f secret.txt config.ini

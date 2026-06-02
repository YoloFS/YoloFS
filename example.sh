#!/bin/bash
# YoloFS CLI walkthrough — stages, snapshots, travel, and permission rules.
# Runs inside example/ so it uses that directory's yolofs.toml.
set -euo pipefail

cd "$(dirname "$0")/example"

section() { echo; echo "════════════════════════════════════════════════════════════"; echo "  $1"; echo "════════════════════════════════════════════════════════════"; echo; }
# Dimmed narration line — explains what to look for in the output that follows.
note() { printf '\033[2m# %s\033[0m\n' "$*"; }
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
note "yolofs stacks a writable overlay over this directory; the real files stay"
note "untouched until you explicitly commit."
run yolo reload
run yolo init
run yolo mount

# ─── Stage a change, inspect it, then discard ──────────────────────

section "Stage a change, inspect it, then discard"
note "Writes inside the mount land in a staging layer, not on the real file."
run yolo exec -- sh -c 'echo hello > greeting.txt'
note "'status' summarizes staged changes; 'diff' shows them line-by-line."
run yolo status
run yolo diff
note "'abort' discards the staging, leaving the directory exactly as it was."
run yolo abort --force

# ─── Build up snapshots ──────────────────────────────────────────

section "Build up snapshots (each exec auto-creates one)"
note "Every 'yolo exec' checkpoints its result, so each command is a point in"
note "time you can return to later."
run yolo exec -- sh -c 'echo step1 > step1.txt'
run yolo exec -- sh -c 'echo step2 > step2.txt'
run yolo exec -- sh -c 'echo step3 > step3.txt'
note "'audit' lists history (latest snapshot by default; --full for everything)."
run yolo audit

# ─── Query across snapshots ──────────────────────────────────────

section "Query across snapshots without leaving the present"
note "Inspect any single snapshot with --at, or a range with --from/--to."
run yolo status --at 2
run yolo diff   --from 1 --to 3

# ─── Travel to an earlier snapshot ──────────────────────────────

section "Travel to snapshot 1"
run yolo exec -- sh -c 'ls step*.txt'
note "Travel rewinds the working state to snapshot 1; snapshots after it become"
note "unreachable (shown dimmed in the audit), and new work branches from here."
run yolo travel 1
run yolo exec -- sh -c 'ls step*.txt'
run yolo exec -- sh -c 'echo step2_new > step2_new.txt'
run yolo audit

# ─── Permission rules: blocked + ask are recorded ──────────────────

section "Permission rules: denied and ask accesses are recorded"
# Rules can only target paths that exist, so create the files first.
echo "secret"  > secret.txt
echo "k = v"   > config.ini
note "Rules gate access by path: 'deny' blocks outright, 'ask' defers to a"
note "permission daemon — and with no daemon running, an ask is denied."
run yolo rule deny secret.txt
run yolo rule ask  config.ini
run yolo exec -- sh -c 'cat secret.txt' || true
run yolo exec -- sh -c 'cat config.ini' || true
note "Each denied/ask attempt leaves an observational note. It shows in 'status'"
note "under \"Observed accesses\" and in the audit, but never counts as a change."
run yolo status
run yolo audit

# ─── Permission daemon: 'yolo watch' answers asks live ─────────────

section "Permission daemon: 'yolo watch' answers an ask"
echo "token123" > apikey.txt
run yolo rule ask apikey.txt
note "'yolo watch' is the permission daemon. Interactively it prompts"
note "[a]llow/[r]ead/[h]ide/[d]eny per request; here we run it with --allow-all"
note "in the background so the walkthrough stays hands-free."
echo "\$ yolo watch --allow-all &"
yolo watch --allow-all &
watch_pid=$!
sleep 0.3
note "With the daemon answering, the ask now resolves to allow and the read"
note "succeeds — contrast the no-daemon deny above."
run yolo exec -- sh -c 'cat apikey.txt'
kill "$watch_pid" 2>/dev/null || true
wait "$watch_pid" 2>/dev/null || true
note "The audit's note for apikey.txt now reads \"ask read … → allow\"."
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
yolo rule unset apikey.txt >/dev/null 2>&1
rm -f secret.txt config.ini apikey.txt

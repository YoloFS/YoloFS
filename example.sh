#!/bin/bash
# YoloFS walkthrough: run commands in a staging overlay, see what changed, then
# keep or discard — plus permission gating and time travel. Uses the
# yolofs.toml in this directory.
set -euo pipefail
cd "$(dirname "$0")/example"

bar='──────────────────────────────────────────────────────────'
section() { printf '\n%s\n  %s\n%s\n' "$bar" "$1" "$bar"; }
note()    { printf '\033[2m# %s\033[0m\n' "$*"; }
# Echo the command as typed (quoting args that contain spaces), then run it.
run() {
  local shown="" a
  for a in "$@"; do
    case "$a" in *" "*) shown+=" '$a'" ;; *) shown+=" $a" ;; esac
  done
  printf '\n\033[1m$%s\033[0m\n' "$shown"; "$@"
}

section "Setup"
run yolo reload
run yolo mount

section "Run a command, see what changed, then keep or discard it"
note "'yolo -- <cmd>' runs <cmd> in a staging overlay and shows what changed."
run yolo -- sh -c 'echo "hello, yolofs" > greeting.txt'
note "'commit' writes the staged change out to the real file."
run yolo commit
run cat greeting.txt
note "'abort' throws staging away instead — the real directory is never touched."
run yolo -- sh -c 'echo oops > mistake.txt'
run yolo abort --force

section "Permission rules & the 'yolo watch' daemon"
echo secret > secret.txt
echo token  > apikey.txt
note "'deny' blocks a path; the attempt is logged under 'Observed accesses'."
run yolo rule deny secret.txt
run yolo -- sh -c 'cat secret.txt' || true
note "'ask' defers to the 'yolo watch' daemon (no daemon => denied). Interactively"
note "it prompts [a]llow/[r]ead/[h]ide/[d]eny; here it runs --allow-all in the background."
run yolo rule ask apikey.txt
printf '\n\033[1m$ yolo watch --allow-all &\033[0m\n'
yolo watch --allow-all & watch_pid=$!
sleep 0.3
run yolo -- sh -c 'cat apikey.txt'
kill "$watch_pid" 2>/dev/null || true
wait "$watch_pid" 2>/dev/null || true

section "Snapshots & time travel"
note "Every run auto-checkpoints, so you can rewind to any earlier point."
run yolo -- sh -c 'echo v1 > notes.txt'
run yolo -- sh -c 'echo v2 >> notes.txt'
run yolo timeline
note "'travel' rewinds the working tree to snapshot 1 ('yolo exec' runs quietly)."
run yolo travel 1
run yolo exec -- cat notes.txt

section "Teardown"
run yolo abort --force
run yolo unmount
run yolo unload
# Leave example/ pristine: drop the demo rules (set+unset is byte-neutral) and
# remove the files the walkthrough created.
yolo rule unset secret.txt >/dev/null 2>&1 || true
yolo rule unset apikey.txt >/dev/null 2>&1 || true
rm -f greeting.txt secret.txt apikey.txt notes.txt

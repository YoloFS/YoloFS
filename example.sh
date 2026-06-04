#!/bin/bash
# YoloFS walkthrough: run commands in a staging overlay, review what changed,
# rewind through snapshots, and gate access with permission rules. Uses the
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

section "Stage a change, review it, then keep or discard"
note "'yolo -- <cmd>' runs <cmd> in a staging overlay and prints what it changed."
run yolo -- sh -c 'echo "hello, yolofs" > greeting.txt'
note "'review --diff' shows the staged changes as a git-style diff."
run yolo review --diff
note "'commit' writes staging out to the real files."
run yolo commit
run cat greeting.txt
note "'abort' throws staging away instead — the real directory is never touched."
run yolo -- sh -c 'echo oops > mistake.txt'
run yolo abort --force

section "Snapshots, history & travel"
note "Every run auto-snapshots, so you can review across them and rewind."
run yolo -- sh -c 'echo step1 > step1.txt'
run yolo -- sh -c 'echo step2 > step2.txt'
note "'review all' lists everything since base; add '--diff' for the content."
run yolo review all
run yolo review all --diff
note "'travel' rewinds the working tree to a snapshot — step2 disappears."
run yolo travel 1
run yolo -- sh -c 'ls step*.txt'
note "'timeline' is the snapshot graph; after travel the abandoned branch is dimmed."
run yolo timeline

section "Permission rules & the 'yolo watch' daemon"
echo secret > secret.txt
echo token  > apikey.txt
note "'deny' blocks a path; the attempt is logged as an access note."
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

section "Teardown"
run yolo abort --force
run yolo unmount
run yolo unload
# Leave example/ pristine: drop the demo rules (set+unset is byte-neutral) and
# remove the files the walkthrough created.
yolo rule unset secret.txt >/dev/null 2>&1 || true
yolo rule unset apikey.txt >/dev/null 2>&1 || true
rm -f greeting.txt secret.txt apikey.txt step1.txt step2.txt mistake.txt

#!/bin/bash
# YoloFS walkthrough: run commands in a staging overlay, review what changed,
# rewind through snapshots, and gate access with permission rules. Scaffolds a
# throwaway project with `yolo init`, runs in it, then removes it on teardown.
set -euo pipefail
cd "$(dirname "$0")"

bar='──────────────────────────────────────────────────────────'
section() { printf '\n%s\n  %s\n%s\n' "$bar" "$1" "$bar"; }
# A blank line precedes the note block; each arg is its own '# ' line. No
# trailing newline, so the note hugs the command printed right after it.
note() {
  local sep="" line
  printf '\n'
  for line in "$@"; do
    printf '%s\033[2m# %s\033[0m' "$sep" "$line"
    sep=$'\n'
  done
}
# Echo the command as typed (quoting args that contain spaces), then run it.
run() {
  local shown="" a
  for a in "$@"; do
    case "$a" in *" "*) shown+=" '$a'" ;; *) shown+=" $a" ;; esac
  done
  printf '\n\033[1m$%s\033[0m\n' "$shown"; "$@"
}

section "Setup"
note "'yolo init <dir>' scaffolds a project: a default yolofs.toml plus agent hook templates."
run yolo init example --agents claude
cd example
run yolo reload
run yolo mount

section "Stage a change, review it, then commit"
note "'yolo -- <cmd>' runs <cmd> in a staging overlay and prints what it changed."
run yolo -- sh -c 'echo "hello, yolofs" > greeting.txt'
note "'review --diff' shows the staged changes as a git-style diff."
run yolo review --diff
note "'commit' writes staging out to the real files."
run yolo commit
run cat greeting.txt

section "Discard a change with abort"
note "'abort' throws staging away instead — the real directory is never touched."
run yolo -- sh -c 'echo oops > mistake.txt'
run yolo abort --force
note "mistake.txt never reached the real directory:"
run ls mistake.txt || true

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
note "'ask' defers to the 'yolo watch' daemon (no daemon => denied). Interactively" \
     "it prompts allow [y]es/[d]eny; here it runs --allow-all in the background."
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
# example/ was scaffolded by `yolo init` above and is git-ignored — drop the
# whole thing so the tree is clean.
cd ..
rm -rf example

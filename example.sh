#!/bin/bash
set -euo pipefail
cd "$(dirname "$0")"

# Mirror everything to example.out, stripping ANSI colors from the file copy so
# it stays readable in a plain editor (the terminal still gets color).
rm -f example.out
exec > >(tee >(sed -E "s/\x1B\[[0-9;]*[a-zA-Z]//g" >> example.out)) 2>&1

# `note` prints narration as a `#` comment line. The DEBUG trap echoes each
# command right before it runs, so example.out reads like a terminal session:
# $BASH_COMMAND is the *unexpanded* source, so quoting is preserved. Bash does
# not fire the DEBUG trap for commands inside functions (functrace is off), so
# `note` never traces its own internals; the case below skips the few infra
# commands (the backgrounded daemon, sleep/kill/wait) we'd rather not show.
note() { printf '\033[34m# %s\033[0m\n' "$*"; }   # blue, distinct from the CLI's dim/green/cyan
cmd_echo() {
    case "$BASH_COMMAND" in
        note\ *|cmd_echo|true|sleep\ *|kill\ *|wait\ *|watch_pid=*|yolo\ watch\ *) ;;
        *) printf '\n\033[1m$ %s\033[0m\n' "$BASH_COMMAND" ;;   # blank line groups each command + its output
    esac
    return 0  # a non-zero DEBUG trap would make bash skip the next command
}
trap cmd_echo DEBUG

note ──────────────────────────────────────────────────────────
note Setup
note ──────────────────────────────────────────────────────────
note "'yolo init <dir>' scaffolds a project: a default yolofs.toml plus agent hook templates."
yolo init example --agents claude
cd example
yolo mount

note ──────────────────────────────────────────────────────────
note Stage, review and commit
note ──────────────────────────────────────────────────────────
note "'yolo run -- <cmd>' runs <cmd> in a staging overlay and prints what it changed."
yolo run -- sh -c 'echo "hello, yolofs" > greeting.txt'
note "'review --diff' shows the staged changes as a git-style diff."
yolo review --diff
note "'commit' writes staging out to the real files."
yolo commit
cat greeting.txt

note ──────────────────────────────────────────────────────────
note Stage and abort
note ──────────────────────────────────────────────────────────
note "'abort' throws staging away instead — the real directory is never touched."
yolo run -- sh -c 'echo oops > mistake.txt'
yolo abort --force
note "mistake.txt never reached the real directory:"
ls mistake.txt || true

note ──────────────────────────────────────────────────────────
note Snapshots, history and travel
note ──────────────────────────────────────────────────────────
note "Every run auto-snapshots, so you can review across them and rewind."
yolo run -- sh -c 'echo step1 > step1.txt'
yolo run -- sh -c 'echo step2 > step2.txt'
note "'review all' lists everything since base; add '--diff' for the content."
yolo review all
yolo review all --diff
note "'travel' rewinds the working tree to a snapshot — step2 disappears."
yolo travel 1
yolo run -- sh -c 'ls step*.txt'
note "'timeline' is the snapshot graph; after travel the abandoned branch is dimmed."
yolo timeline

note ──────────────────────────────────────────────────────────
note "Permission rules and the 'yolo watch' daemon"
note ──────────────────────────────────────────────────────────
echo secret > secret.txt
echo token  > apikey.txt
note "'deny' blocks a path; the attempt is logged as an access #."
yolo rule deny secret.txt
yolo run -- sh -c 'cat secret.txt' || true
note "'ask' defers to the 'yolo watch' daemon (no daemon => denied). Interactively"
note "it prompts allow [y]es/[d]eny; here it runs --allow-all in the background."
yolo rule ask apikey.txt
printf '\n\033[1m$ yolo watch --allow-all &\033[0m\n'
yolo watch --allow-all & watch_pid=$!
sleep 0.3
yolo run -- sh -c 'cat apikey.txt'
kill "$watch_pid" 2>/dev/null || true
wait "$watch_pid" 2>/dev/null || true

note ──────────────────────────────────────────────────────────
note Teardown
note ──────────────────────────────────────────────────────────
yolo abort --force
yolo unmount
yolo unload
# example/ was scaffolded by `yolo init` above and is git-ignored — drop the
# whole thing so the tree is clean.
cd ..
rm -rf example

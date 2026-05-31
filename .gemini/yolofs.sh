#!/usr/bin/env bash
# BeforeTool hook for run_shell_command: rewrites commands to run through
# the yolofs sandbox.
#
# Input (stdin): JSON with tool_input.command
# Output (stdout): JSON with hookSpecificOutput.tool_input rewriting the command
set -euo pipefail

input=$(cat)
command=$(echo "$input" | jq -r '.tool_input.command // ""')
commit_pattern='(^|[^[:alnum:]_-])yolo[[:space:]]+commit([[:space:]]|$)'

if [[ "$command" =~ $commit_pattern ]]; then
  updated_command="printf '%s\n' 'yolo commit is reserved for human approval; leave staged changes for review.'; yolo status; exit 2"
elif [[ "$command" =~ ^[[:space:]]*yolo([[:space:]]|$) ]]; then
  updated_command="$command"
else
  updated_command="yolo exec -- $command"
fi

jq -n --arg cmd "$updated_command" '{
  hookSpecificOutput: {
    tool_input: { command: $cmd }
  }
}'

#!/usr/bin/env bash
# PreToolUse hook for Bash: rewrites commands to run through yolofs sandbox.
#
# Input (stdin): JSON with tool_input.command
# Output (stdout): JSON with updatedInput to rewrite the command
#
# This wraps every Bash command in yolofs so the agent can see filesystem
# changes and decide whether to leave them for human approval or abort them.
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
    hookEventName: "PreToolUse",
    permissionDecision: "allow",
    updatedInput: { command: $cmd },
    permissionDecisionReason: "Wrapped in yolofs sandbox for visibility and reversal"
  }
}'

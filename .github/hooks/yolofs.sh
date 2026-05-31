#!/usr/bin/env bash
# preToolUse hook for Copilot CLI's bash tool: rewrites commands to run
# through the yolofs sandbox.
#
# Input (stdin): JSON with toolName, toolArgs (toolArgs is a JSON-encoded string).
# Output (stdout): JSON with permissionDecision + modifiedArgs.command.
#
# This wraps every bash command in yolofs so the agent can see filesystem
# changes and decide whether to leave them for human approval or abort them.
set -euo pipefail

input=$(cat)
tool_name=$(echo "$input" | jq -r '.toolName // ""')

if [ "$tool_name" != "bash" ]; then
  jq -n '{permissionDecision: "allow"}'
  exit 0
fi

command=$(echo "$input" | jq -r '.toolArgs // "{}"' | jq -r '.command // ""')
commit_pattern='(^|[^[:alnum:]_-])yolo[[:space:]]+commit([[:space:]]|$)'

if [[ "$command" =~ $commit_pattern ]]; then
  updated_command="printf '%s\n' 'yolo commit is reserved for human approval; leave staged changes for review.'; yolo status; exit 2"
elif [[ "$command" =~ ^[[:space:]]*yolo([[:space:]]|$) ]]; then
  updated_command="$command"
else
  updated_command="yolo exec -- $command"
fi

jq -n --arg cmd "$updated_command" '{
  permissionDecision: "allow",
  modifiedArgs: { command: $cmd },
  permissionDecisionReason: "Wrapped in yolofs sandbox for visibility and reversal"
}'

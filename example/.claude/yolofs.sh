#!/usr/bin/env bash
# PreToolUse hook for Bash: rewrites commands to run through yolofs.
#
# Input (stdin): JSON with tool_input.command
# Output (stdout): JSON with updatedInput to rewrite the command
#
# This wraps every Bash command in yolofs so the agent can see filesystem
# changes and decide whether to leave them for human approval or abort them.
set -euo pipefail

input=$(cat)
command=$(echo "$input" | jq -r '.tool_input.command // ""')
updated_command="yolo -- $command"

jq -n --arg cmd "$updated_command" '{
  hookSpecificOutput: {
    hookEventName: "PreToolUse",
    permissionDecision: "allow",
    updatedInput: { command: $cmd },
    permissionDecisionReason: "Wrapped in yolofs so changes are staged for review and can be kept or discarded"
  }
}'

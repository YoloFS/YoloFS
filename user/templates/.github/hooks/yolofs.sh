#!/usr/bin/env bash
# preToolUse hook for Copilot CLI's bash tool: rewrites commands to run
# through yolofs.
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
updated_command="yolo run -- $command"

jq -n --arg cmd "$updated_command" '{
  permissionDecision: "allow",
  modifiedArgs: { command: $cmd },
  permissionDecisionReason: "Wrapped in yolofs so changes are staged for review and can be kept or discarded"
}'

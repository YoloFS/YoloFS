#!/usr/bin/env bash
# PreToolUse hook for Bash: rewrites commands to run through agfs sandbox.
#
# Input (stdin): JSON with tool_input.command
# Output (stdout): JSON with updatedInput to rewrite the command
#
# This wraps every Bash command in agfs so the agent can see filesystem
# changes and decide whether to commit or abort them.
set -euo pipefail

AGFS="$(cd "$(dirname "$0")/.." && pwd)/target/release/agfs"

input=$(cat)
command=$(echo "$input" | jq -r '.tool_input.command // ""')

# Don't wrap agfs commands themselves (avoid infinite recursion)
if echo "$command" | grep -q 'agfs'; then
  exit 0
fi

# Don't wrap read-only commands
if echo "$command" | grep -qE '^(ls|cat|head|tail|wc|echo|pwd|which|file|find|grep|less|more|tree|stat|du|df|env|printenv|id|whoami|uname|date|readlink)(\s|$)'; then
  exit 0
fi

# Rewrite the command to run through agfs
updated_command="$AGFS --auto-stage -- $command"

jq -n --arg cmd "$updated_command" '{
  hookSpecificOutput: {
    hookEventName: "PreToolUse",
    permissionDecision: "allow",
    updatedInput: { command: $cmd },
    permissionDecisionReason: "Wrapped in agfs sandbox for visibility and reversal"
  }
}'

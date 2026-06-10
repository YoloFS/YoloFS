#!/usr/bin/env bash
# BeforeTool hook for run_shell_command: rewrites commands to run through
# yolofs.
#
# Input (stdin): JSON with tool_input.command
# Output (stdout): JSON with hookSpecificOutput.tool_input rewriting the command
set -euo pipefail

input=$(cat)
command=$(echo "$input" | jq -r '.tool_input.command // ""')
updated_command="yolo run -- $command"

jq -n --arg cmd "$updated_command" '{
  hookSpecificOutput: {
    tool_input: { command: $cmd }
  }
}'

#!/usr/bin/env bash
set -euo pipefail

if [[ $# -lt 1 || $# -gt 2 ]]; then
  echo "usage: $0 <agent-command> [comma-separated-agent-args]" >&2
  exit 2
fi

agent_command=$1
agent_args=${2:-acp}
exec timeout --signal=TERM --kill-after=2s 15s \
  buzz-acp models --json \
  --agent-command "$agent_command" \
  --agent-args "$agent_args"

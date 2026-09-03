#!/bin/sh
# Lights-off Claude Code worker. Instruction arrives in WANAX_INSTRUCTION, not argv.
set -eu
if [ -z "${WANAX_INSTRUCTION:-}" ]; then
  echo "wanax cmd worker: WANAX_INSTRUCTION is empty" >&2
  exit 1
fi
# stdin keeps the task out of `ps` for this wrapper.
printf '%s' "$WANAX_INSTRUCTION" | claude -p --dangerously-skip-permissions

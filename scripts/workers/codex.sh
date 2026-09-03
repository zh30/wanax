#!/bin/sh
# Lights-off Codex worker. Instruction arrives in WANAX_INSTRUCTION, not argv.
set -eu
if [ -z "${WANAX_INSTRUCTION:-}" ]; then
  echo "wanax cmd worker: WANAX_INSTRUCTION is empty" >&2
  exit 1
fi
printf '%s' "$WANAX_INSTRUCTION" | codex exec --sandbox workspace-write -

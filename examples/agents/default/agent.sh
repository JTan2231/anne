#!/usr/bin/env bash
set -euo pipefail

# Read stdin once so we can log what we are sending to Codex.
prompt=$(cat)
if [ -z "$prompt" ]; then
  printf '[default shim] prompt: <empty>\n' >&2
else
  first_line=${prompt%%$'\n'*}
  printf '[default shim] prompt (first line preview): %s\n' "$first_line" >&2
fi

printf '%s' "$prompt" | codex exec --json -

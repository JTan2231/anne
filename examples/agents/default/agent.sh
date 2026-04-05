#!/usr/bin/env bash
set -euo pipefail

prompt_file=$(
  mktemp "${TMPDIR:-/tmp}/anne-default-agent.XXXXXX"
) || {
  printf '[default shim] failed creating prompt staging file\n' >&2
  exit 1
}

cleanup() {
  rm -f "$prompt_file"
}
trap cleanup EXIT

if ! cat >"$prompt_file"; then
  printf '[default shim] failed staging prompt bytes\n' >&2
  exit 1
fi

if [ ! -s "$prompt_file" ]; then
  printf '[default shim] prompt: <empty>\n' >&2
else
  IFS= read -r first_line <"$prompt_file" || true
  printf '[default shim] prompt (first line preview): %s\n' "$first_line" >&2
fi

codex exec --json - <"$prompt_file"

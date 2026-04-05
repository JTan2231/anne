#!/bin/sh
set -eu

repo_root=$(CDPATH= cd -- "$(dirname "$0")" && pwd)
cd "$repo_root"

git2_features_file=$(mktemp)
git2_incoming_features_file=$(mktemp)
default_tree_file=$(mktemp)
cleanup() {
  rm -f "$git2_features_file" "$git2_incoming_features_file" "$default_tree_file"
}
trap cleanup EXIT HUP INT TERM

printf '==> Checking default git2 feature surface\n'
cargo tree --locked -e features -p git2 >"$git2_features_file"
cargo tree --locked -e features -i git2 >"$git2_incoming_features_file"

reject_feature() {
  feature=$1
  if grep -F "$feature" "$git2_features_file" >/dev/null 2>&1; then
    printf 'unexpected git2 feature in default graph: %s\n' "$feature" >&2
    cat "$git2_features_file" >&2
    exit 1
  fi
}

reject_feature 'git2 feature "default"'
reject_feature 'git2 feature "https"'
reject_feature 'git2 feature "ssh"'
if ! grep -F 'git2 feature "vendored-libgit2"' "$git2_incoming_features_file" >/dev/null 2>&1; then
  printf 'missing required git2 feature in default graph: git2 feature "vendored-libgit2"\n' >&2
  cat "$git2_incoming_features_file" >&2
  exit 1
fi

printf '==> Checking default dependency graph for remote transport crates\n'
cargo tree --locked -e all --prefix none >"$default_tree_file"

reject_crate() {
  crate=$1
  if grep -F "$crate v" "$default_tree_file" >/dev/null 2>&1; then
    printf 'unexpected crate in default dependency graph: %s\n' "$crate" >&2
    grep -F "$crate v" "$default_tree_file" >&2
    exit 1
  fi
}

reject_crate 'openssl-sys'
reject_crate 'libssh2-sys'

printf '==> Building anne\n'
cargo build --locked

printf '==> Running anne tests\n'
cargo test --locked

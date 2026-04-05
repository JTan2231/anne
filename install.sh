#!/bin/sh
set -eu

usage() {
  cat <<'EOF'
Usage: ./install.sh [--dry-run] [--uninstall]

Build and install Anne from a clone.

Environment variables:
  PREFIX   Install prefix (default: /usr/local)
  DESTDIR  Staging root for packaging (default: empty)
  CARGO_TARGET_DIR  Cargo build target dir (default: target; root uses a temp dir)
  BINDIR   Install dir for the anne binary (default: $PREFIX/bin)
  DATADIR  Install dir for shared data (default: $PREFIX/share)
  AGENTSDIR  Install dir for bundled agent shims
             (default: $DATADIR/anne/agents)

Install layout:
  $BINDIR/anne
  $AGENTSDIR/<label>/{agent.sh,filter.sh,...}
  $DATADIR/anne/install-manifest.txt

Notes:
  - For system prefixes, run as root or via sudo (this script never invokes sudo).
  - The bundled default filter script requires jq.
  - When the repo does not ship Cargo.lock, this script generates it for a
    deterministic build of the current dependency graph.
EOF
}

say() {
  printf '%s\n' "$*"
}

die() {
  printf 'error: %s\n' "$*" 1>&2
  exit 1
}

script_dir=$(CDPATH= cd "$(dirname "$0")" && pwd)
cd "$script_dir"

dry_run=0
uninstall=0

while [ $# -gt 0 ]; do
  case "$1" in
    --dry-run)
      dry_run=1
      ;;
    --uninstall)
      uninstall=1
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      die "unknown argument: $1"
      ;;
  esac
  shift
done

if [ "$dry_run" -eq 1 ] && [ "$uninstall" -eq 1 ]; then
  die "--dry-run and --uninstall cannot be combined"
fi

PREFIX=${PREFIX:-/usr/local}
DESTDIR=${DESTDIR:-}
BINDIR=${BINDIR:-"$PREFIX/bin"}
DATADIR=${DATADIR:-"$PREFIX/share"}
AGENTSDIR=${AGENTSDIR:-"$DATADIR/anne/agents"}

agents_src="examples/agents"
manifest_rel="$DATADIR/anne/install-manifest.txt"
manifest_path="$DESTDIR$manifest_rel"
temp_target_dir=""

is_writable_parent() {
  target="$1"
  dir="$target"

  while [ ! -d "$dir" ]; do
    parent=$(dirname "$dir")
    if [ "$parent" = "$dir" ]; then
      break
    fi
    dir="$parent"
  done

  [ -w "$dir" ]
}

check_install_permissions() {
  if [ "$dry_run" -eq 1 ] || [ "$uninstall" -eq 1 ]; then
    return 0
  fi

  if [ "$(id -u)" -eq 0 ]; then
    return 0
  fi

  unwritable=""
  for path in \
    "$DESTDIR$BINDIR" \
    "$DESTDIR$DATADIR" \
    "$DESTDIR$AGENTSDIR" \
    "$(dirname "$manifest_path")"
  do
    if ! is_writable_parent "$path"; then
      unwritable="${unwritable}  $path\n"
    fi
  done

  if [ -n "$unwritable" ]; then
    {
      printf 'error: install destination is not writable\n'
      printf 'The following paths are not writable by the current user:\n'
      printf '%b' "$unwritable"
      printf '\nTry one of:\n'
      printf '  - sudo ./install.sh\n'
      printf '  - PREFIX="$HOME/.local" ./install.sh\n'
      printf '  - DESTDIR=/path/to/stage PREFIX=/usr/local ./install.sh\n'
    } 1>&2
    exit 1
  fi
}

run() {
  if [ "$dry_run" -eq 1 ]; then
    say "+ $*"
    return 0
  fi
  "$@"
}

setup_cargo_target_dir() {
  target_dir=${CARGO_TARGET_DIR:-target}

  if [ -n "${CARGO_TARGET_DIR:-}" ]; then
    return 0
  fi

  if [ "$(id -u)" -eq 0 ]; then
    if [ "$dry_run" -eq 1 ]; then
      target_dir="${TMPDIR:-/tmp}/anne-install-target.XXXXXX"
      return 0
    fi

    temp_target_dir=$(mktemp -d "${TMPDIR:-/tmp}/anne-install-target.XXXXXX")
    target_dir="$temp_target_dir"
    export CARGO_TARGET_DIR="$target_dir"
  fi
}

cleanup_temp_target_dir() {
  if [ -n "$temp_target_dir" ] && [ -d "$temp_target_dir" ]; then
    rm -rf "$temp_target_dir"
  fi
}

trap cleanup_temp_target_dir EXIT

install_dir() {
  run install -d "$1"
}

install_file() {
  mode="$1"
  src="$2"
  dst="$3"

  install_dir "$(dirname "$dst")"
  run install -m "$mode" "$src" "$dst"
}

if [ "$uninstall" -eq 1 ]; then
  if [ ! -f "$manifest_path" ]; then
    die "manifest not found: $manifest_path"
  fi

  while IFS= read -r relpath; do
    [ -n "$relpath" ] || continue
    rm -f "$DESTDIR$relpath"
  done <"$manifest_path"

  rm -f "$manifest_path"

  if [ -d "$DESTDIR$AGENTSDIR" ]; then
    find "$DESTDIR$AGENTSDIR" -type d -depth -exec rmdir {} \; 2>/dev/null || true
  fi
  rmdir "$DESTDIR$DATADIR/anne" 2>/dev/null || true

  say "Uninstalled files listed in $manifest_rel"
  exit 0
fi

if [ ! -d "$agents_src" ]; then
  die "missing agent shims directory: $agents_src"
fi

agent_files=$(find "$agents_src" -type f | sort)
if [ -z "$agent_files" ]; then
  die "no agent shim files found under $agents_src"
fi

check_install_permissions

setup_cargo_target_dir
bin_src="$target_dir/release/anne"

if [ "$dry_run" -eq 1 ]; then
  say "+ cargo generate-lockfile (if Cargo.lock missing)"
  say "+ cargo build --locked --release -p anne"
else
  if [ ! -f Cargo.lock ]; then
    say "Generating Cargo.lock for a deterministic build..." 1>&2
    cargo generate-lockfile
  fi
  cargo build --locked --release -p anne
fi

if [ "$dry_run" -eq 0 ] && [ ! -f "$bin_src" ]; then
  die "built binary not found: $bin_src (set CARGO_TARGET_DIR or build manually)"
fi

installed_paths=""
installed_agent_targets=""

record_manifest_path() {
  installed_paths="${installed_paths}${1}\n"
}

install_file 0755 "$bin_src" "$DESTDIR$BINDIR/anne"
record_manifest_path "$BINDIR/anne"

for src in $agent_files; do
  rel=${src#"$agents_src"/}
  dst_rel="$AGENTSDIR/$rel"
  install_file 0755 "$src" "$DESTDIR$dst_rel"
  record_manifest_path "$dst_rel"
  installed_agent_targets="${installed_agent_targets}  $dst_rel\n"
done

install_dir "$(dirname "$manifest_path")"
if [ "$dry_run" -eq 1 ]; then
  say "+ write manifest $manifest_rel"
else
  printf "%b" "$installed_paths" | sort -u >"$manifest_path"
fi

say "Installed:"
say "  $BINDIR/anne"
say "Bundled agent shims:"
printf "%b" "$installed_agent_targets"
say "Manifest:"
say "  $manifest_rel"

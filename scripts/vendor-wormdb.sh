#!/usr/bin/env bash
# Copy a WormDB release binary into src-tauri/binaries for bundling.
# Usage:
#   ./scripts/vendor-wormdb.sh
#   ./scripts/vendor-wormdb.sh /path/to/wormdb
#   WORMDB_SRC=/path/to/wormdb ./scripts/vendor-wormdb.sh

set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
DEST_DIR="$ROOT/src-tauri/binaries"
SRC="${1:-${WORMDB_SRC:-}}"

triple() {
  local arch os
  arch="$(uname -m)"
  case "$arch" in
    x86_64|amd64) arch="x86_64" ;;
    arm64|aarch64) arch="aarch64" ;;
  esac
  os="$(uname -s)"
  case "$os" in
    Darwin) echo "${arch}-apple-darwin" ;;
    Linux) echo "${arch}-unknown-linux-gnu" ;;
    *) echo "${arch}-unknown-unknown" ;;
  esac
}

if [[ -z "$SRC" ]]; then
  for c in \
    "$HOME/src/wormdb/zig-out/bin/wormdb" \
    "$HOME/wormdb/zig-out/bin/wormdb" \
    "/opt/wormdb/wormdb"
  do
    if [[ -f "$c" ]]; then SRC="$c"; break; fi
  done
fi

if [[ -z "$SRC" || ! -f "$SRC" ]]; then
  echo "No WormDB binary found. Build WormDB and pass path or set WORMDB_SRC." >&2
  exit 1
fi

mkdir -p "$DEST_DIR"
T="$(triple)"
cp -f "$SRC" "$DEST_DIR/wormdb-$T"
cp -f "$SRC" "$DEST_DIR/wormdb"
chmod +x "$DEST_DIR/wormdb-$T" "$DEST_DIR/wormdb"

for cfg in debug release; do
  out="$ROOT/target/$cfg"
  if [[ -d "$out" ]]; then
    cp -f "$SRC" "$out/wormdb"
    chmod +x "$out/wormdb"
  fi
done

echo "Vendored WormDB:"
ls -la "$DEST_DIR/wormdb-$T" "$DEST_DIR/wormdb"

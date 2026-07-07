#!/usr/bin/env bash
# Build the bet compiler + windowed runtime + the DOOM port, then run it.
#
# Usage:
#   ports/doom/run.sh                       # attract loop: title screen -> demos -> menu
#   ports/doom/run.sh -warp 1 1 -skill 3    # skip the title, jump straight into E1M1
#   ports/doom/run.sh -playdemo demo1       # any extra args are passed through to the game
#
# Env overrides:
#   DOOM_WAD=/path/to/doom1.wad   (default: <repo>/doom-reference/doom1.wad)
#   DOOM_BIN=/path/to/output      (default: /tmp/doom)
#   LLVM_SYS_180_PREFIX=...        (default: /opt/homebrew/opt/llvm@18)
set -euo pipefail

# This script lives in ports/doom/ ; resolve the repo root two levels up.
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
cd "$REPO_ROOT"

# Build env for the `llvm` feature on this Mac (see the llvm-local-build-env memory).
export PATH="$HOME/.cargo/bin:$PATH"
export LLVM_SYS_180_PREFIX="${LLVM_SYS_180_PREFIX:-/opt/homebrew/opt/llvm@18}"
export LIBRARY_PATH="/opt/homebrew/lib${LIBRARY_PATH:+:$LIBRARY_PATH}"

WAD="${DOOM_WAD:-$REPO_ROOT/doom-reference/doom1.wad}"
OUT="${DOOM_BIN:-/tmp/doom}"

if [[ ! -f "$WAD" ]]; then
  echo "error: WAD not found at '$WAD' — set DOOM_WAD=/path/to/doom1.wad" >&2
  exit 1
fi

echo "==> building bet compiler (driver, llvm) + runtime (gg-desktop)"
cargo build -p driver  --features llvm
cargo build -p runtime --features gg-desktop

echo "==> compiling the DOOM port -> $OUT"
target/debug/bet build ports/doom/doom.bet --runtime real -o "$OUT"

echo "==> running: $OUT -iwad $WAD $*"
exec "$OUT" -iwad "$WAD" "$@"

#!/usr/bin/env bash
# Build the bet compiler + windowed runtime + the fireworks demo, then run it.
#
# Usage:
#   ports/fireworks/run.sh              # windowed — watch the fireworks
#   HEADLESS=1 ports/fireworks/run.sh   # headless — prints the scratch-arena trace, self-terminates
#
# Env overrides:
#   FIREWORKS_BIN=/path/to/output   (default: /tmp/fireworks)
#   LLVM_SYS_180_PREFIX=...          (default: /opt/homebrew/opt/llvm@18)
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
cd "$REPO_ROOT"

# Build env for the `llvm` feature on this Mac (see the llvm-local-build-env memory).
export PATH="$HOME/.cargo/bin:$PATH"
export LLVM_SYS_180_PREFIX="${LLVM_SYS_180_PREFIX:-/opt/homebrew/opt/llvm@18}"
export LIBRARY_PATH="/opt/homebrew/lib${LIBRARY_PATH:+:$LIBRARY_PATH}"
export GG_SCALE="${GG_SCALE:-2}"

OUT="${FIREWORKS_BIN:-/tmp/fireworks}"

echo "==> building bet compiler (driver, llvm) + runtime (gg-desktop)"
cargo build -p driver  --features llvm
cargo build -p runtime --features gg-desktop

echo "==> compiling the fireworks port -> $OUT"
target/debug/bet build ports/fireworks/fireworks.bet --runtime real -o "$OUT"

if [[ "${HEADLESS:-0}" == "1" ]]; then
  echo "==> running headless: $OUT"
  exec env BET_GG_HEADLESS=1 "$OUT" "$@"
else
  echo "==> running windowed: $OUT"
  exec "$OUT" "$@"
fi

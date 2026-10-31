#!/usr/bin/env bash
# Simulator runner: builds and launches the x86_64 Linux simulator.
# Usage: ./tools/ai-sim.sh [make args...]
# Builds with `make -j10 sim-linux`, then launches the sim binary with the
# standard env vars. Extra args are forwarded to make.
set -euo pipefail

export PATH="$HOME/.cargo/bin:$PATH"

ROOT="$(git rev-parse --show-toplevel 2>/dev/null || pwd)"
cd "$ROOT"

echo "Building simulator..."
make -j10 sim-linux "$@"

BIN="$ROOT/rust-modules/target-sim/release/plxnative-sim"
if [ ! -x "$BIN" ]; then
    echo "Simulator binary not found at $BIN"
    exit 1
fi

export PLXNATIVE_APP_DIR="$ROOT/pkg"
export PLXNATIVE_RUNTIME_DIR="${PLXNATIVE_RUNTIME_DIR:-/tmp/plxnative-sim}"
mkdir -p "$PLXNATIVE_RUNTIME_DIR"

echo "Launching simulator..."
echo "  PLXNATIVE_APP_DIR=$PLXNATIVE_APP_DIR"
echo "  PLXNATIVE_RUNTIME_DIR=$PLXNATIVE_RUNTIME_DIR"
exec "$BIN"
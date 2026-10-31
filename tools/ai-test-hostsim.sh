#!/usr/bin/env bash
# Hostsim test runner: runs the cfg(hostsim) lib tests with the exact environment the
# docs require. Target dir is kept OUTSIDE the checkout. Optional filter args are
# forwarded to cargo (e.g. ./tools/ai-test-hostsim.sh render).
# Usage: ./tools/ai-test-hostsim.sh [filter...]
set -euo pipefail

export CARGO_INCREMENTAL=1
export CARGO_TARGET_DIR=/tmp/ai-hostsim-target
export PLXNATIVE_RUNTIME_DIR="$(mktemp -d)"

cd "$(git rev-parse --show-toplevel 2>/dev/null)/rust-modules" 2>/dev/null || cd rust-modules

echo "CARGO_TARGET_DIR=$CARGO_TARGET_DIR"
echo "PLXNATIVE_RUNTIME_DIR=$PLXNATIVE_RUNTIME_DIR"
echo "Running hostsim tests..."

cargo +nightly test --lib -p plxnative-modules --features hostsim -- "$@"

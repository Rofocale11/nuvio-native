#!/usr/bin/env bash
# Hostsim test runner: a thin wrapper over `make test-hostsim`, which builds into the
# gitignored `rust-modules/target-hostsim` (inside the checkout, so `make disk` /
# `tools/build-gc.sh` own it) and passes `--require-match`, so a filter that matches no
# test fails instead of reporting "0 tests" as success.
# Usage: ./tools/ai-test-hostsim.sh [filter]   (the filter becomes make's T=...)
set -euo pipefail

root="$(git rev-parse --show-toplevel 2>/dev/null || pwd)"
filter="${1:-}"

echo "hostsim: make test-hostsim ${filter:+T=$filter}"
if [ -n "$filter" ]; then
  exec make -C "$root" test-hostsim "T=$filter"
fi
exec make -C "$root" test-hostsim

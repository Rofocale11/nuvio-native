#!/usr/bin/env bash
# Module-layer verifier: runs ci/check-module-layers.py and reports pass/fail.
# Usage: ./tools/ai-layers.sh
set -u

cd "$(git rev-parse --show-toplevel 2>/dev/null || echo .)" || exit 1

if python3 ci/check-module-layers.py; then
    echo "Layer check PASSED"
    exit 0
else
    echo "Layer check FAILED (see forbidden edges above)"
    exit 1
fi
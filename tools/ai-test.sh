#!/usr/bin/env bash
# Change-scoped test runner: runs tests only for the crates touched by the
# current (uncommitted) changes vs HEAD, using cargo-nextest. Nearly instant.
# Usage: ./tools/ai-test.sh
set -u

# Ensure cargo toolchain is on PATH in non-login shells.
export PATH="$HOME/.cargo/bin:$PATH"

if ! command -v cargo-nextest >/dev/null 2>&1; then
    echo "cargo-nextest is not installed. Install it first:"
    echo "    cargo install cargo-nextest --locked"
    exit 1
fi

changed="$(git diff --name-only HEAD 2>/dev/null || true)"
if [ -z "$changed" ]; then
    echo "No changed files vs HEAD."
    exit 0
fi

crates=""
for f in $changed; do
    case "$f" in
        rust-modules/src/*)
            crates="$crates plxnative-modules"
            ;;
        rust-modules/*/*)
            dir="$(printf '%s' "$f" | sed -n 's|^rust-modules/\([^/]*\)/.*|\1|p')"
            if [ -n "$dir" ]; then
                case "$dir" in
                    storage) crates="$crates plxnative-storage" ;;
                    *) crates="$crates plx_$dir" ;;
                esac
            fi
            ;;
    esac
done

# dedup + trim
crates="$(printf '%s' "$crates" | tr ' ' '\n' | grep -v '^$' | sort -u | tr '\n' ' ' | sed 's/ *$//')"

if [ -z "$crates" ]; then
    echo "No Rust crate changes detected."
    exit 0
fi

echo "Changed crates: $crates"

cd rust-modules || { echo "rust-modules not found"; exit 1; }
# Reuse the incremental tree `make test-crate`/`test-fast` build (relative to this dir).
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-target-fast}"
# Tests that honour it get a throwaway runtime root, as the Makefile recipes give them.
if [ -z "${PLXNATIVE_RUNTIME_DIR:-}" ]; then
    PLXNATIVE_RUNTIME_DIR="$(mktemp -d /tmp/plxnative-test.XXXXXX)"
    export PLXNATIVE_RUNTIME_DIR
    trap 'rm -rf "$PLXNATIVE_RUNTIME_DIR"' EXIT
fi
status=0
for c in $crates; do
    echo "=== $c ==="
    # One crate's failure must not hide another's.
    cargo nextest run --no-fail-fast -p "$c" || status=1
done
exit $status
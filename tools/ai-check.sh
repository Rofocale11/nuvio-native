#!/usr/bin/env bash
# Start `make check` in the background so the terminal tool's 120s timeout
# does not kill it. Poll progress with tools/ai-check-status.sh.
set -euo pipefail

LOG=/tmp/ai-check.log
PIDFILE=/tmp/ai-check.pid

if [[ -f "$PIDFILE" ]]; then
    old_pid="$(cat "$PIDFILE" 2>/dev/null || true)"
    if [[ -n "${old_pid:-}" ]] && kill -0 "$old_pid" 2>/dev/null; then
        echo "A check is already running (PID $old_pid). Use tools/ai-check-status.sh to poll."
        exit 0
    fi
fi

: > "$LOG"
nohup make check > "$LOG" 2>&1 &
echo $! > "$PIDFILE"
echo "Check started in background"

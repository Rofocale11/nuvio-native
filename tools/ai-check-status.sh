#!/usr/bin/env bash
# Report whether the background `make check` (started by tools/ai-check.sh) is
# still running, or print its `test result:` lines once it has finished.
set -u

LOG=/tmp/ai-check.log
PIDFILE=/tmp/ai-check.pid

running=0
if [[ -f "$PIDFILE" ]]; then
    pid="$(cat "$PIDFILE" 2>/dev/null || true)"
    if [[ -n "${pid:-}" ]] && kill -0 "$pid" 2>/dev/null; then
        running=1
    fi
fi

if [[ "$running" -eq 0 ]] && pgrep -f "make check" >/dev/null 2>&1; then
    running=1
fi

if [[ "$running" -eq 1 ]]; then
    echo "STATUS: RUNNING"
    exit 0
fi

if [[ -f "$LOG" ]]; then
    grep "test result:" "$LOG" || echo "(no 'test result:' line found in $LOG)"
else
    echo "No log found at $LOG"
fi

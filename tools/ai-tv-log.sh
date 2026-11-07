#!/bin/bash
TV_IP=$(cat .tv-host)
if [ -z "$TV_IP" ]; then
    echo "Error: .tv-host is empty"
    exit 1
fi

SSH_CMD="ssh -i ~/.ssh/id_ed25519 root@$TV_IP"

# `/tmp` for the stable install, `/tmp/<app id>` for a developer install beside it
# (paths::resolve_runtime_dir). Both roots carry the SAME file names; reading only `/tmp` silently
# shows the stable install's stale session while a debug build runs beside it, so every mode here
# globs both. `tail`/`grep` print a `==> file <==` header when more than one match exists, which
# is also how a caller tells which install a line came from.
EVENTS="/tmp/plxnative-events.log /tmp/*/plxnative-events.log"
STDERR="/tmp/plxnative-stderr.log /tmp/*/plxnative-stderr.log"
CRASH="/tmp/plxnative-crash.log /tmp/*/plxnative-crash.log"

if [ "$1" == "grep" ]; then
    $SSH_CMD "grep -a -E '$2' $EVENTS $STDERR | tail -n 200" 2>/dev/null
elif [ "$1" == "perf" ]; then
    $SSH_CMD "grep -a -E 'loop=|posters:|imgcache:|coldopen|route=' $EVENTS | tail -n 200" 2>/dev/null
elif [ "$1" == "p2p" ]; then
    # The C engine logs to the event log (`sbengine:` / `decision:` / `stream:` lines);
    # /tmp/serverp2p.log belonged to the Node child and is no longer written.
    LINES=${2:-100}
    $SSH_CMD "grep -a -E 'sbengine:|decision:|stream: GET|ff: open|peers=' $EVENTS | tail -n $LINES" 2>/dev/null
elif [ "$1" == "panic" ]; then
    # Check stderr and crash logs for panics/backtraces
    $SSH_CMD "echo '--- CRASH LOG ---' && cat $CRASH && echo '--- STDERR (Last 100) ---' && tail -n 100 $STDERR" 2>/dev/null
elif [ "$1" == "raw" ]; then
    LINES=${2:-100}
    $SSH_CMD "tail -n $LINES $EVENTS" 2>/dev/null
elif [ "$1" == "mem" ]; then
    # Read-only snapshot of every app process from /proc (VmRSS, peak, swap). A stable and a debug
    # install are two separate processes, each with its own binary path; list them all rather than
    # taking the first, so the cmdline says which build the numbers belong to.
    # `pgrep -x plxnative`, not `-f com.nuvionative.app`: the ssh command line itself carries the
    # pattern, so `-f` matched the shell running this probe and reported ~540 kB every time.
    $SSH_CMD 'PIDS=$(pgrep -x plxnative); if [ -z "$PIDS" ]; then echo "App process not found (is the app running?)"; exit 1; fi; for PID in $PIDS; do echo "PID=$PID cmd=$(tr "\0" " " < /proc/$PID/cmdline)"; grep -E "^(VmRSS|VmHWM|VmSwap):" /proc/$PID/status; done' 2>/dev/null
else
    LINES=${1:-100}
    $SSH_CMD "tail -n $LINES $EVENTS | grep -v 'key type='" 2>/dev/null
fi

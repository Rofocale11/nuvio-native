#!/bin/bash
TV_IP=$(cat .tv-host)
if [ -z "$TV_IP" ]; then
    echo "Error: .tv-host is empty"
    exit 1
fi

SSH_CMD="ssh -i ~/.ssh/id_ed25519 root@$TV_IP"

if [ "$1" == "grep" ]; then
    $SSH_CMD "grep -a -E '$2' /tmp/plxnative-events.log /tmp/plxnative-stderr.log /tmp/nuvio-p2p.log | tail -n 200" 2>/dev/null
elif [ "$1" == "perf" ]; then
    $SSH_CMD "grep -a -E 'loop=|posters:|imgcache:|coldopen|route=' /tmp/plxnative-events.log | tail -n 200" 2>/dev/null
elif [ "$1" == "p2p" ]; then
    LINES=${2:-100}
    $SSH_CMD "tail -n $LINES /tmp/nuvio-p2p.log" 2>/dev/null
elif [ "$1" == "panic" ]; then
    # Check stderr and crash logs for panics/backtraces
    $SSH_CMD "echo '--- CRASH LOG ---' && cat /tmp/plxnative-crash.log && echo '--- STDERR (Last 100) ---' && tail -n 100 /tmp/plxnative-stderr.log" 2>/dev/null
elif [ "$1" == "raw" ]; then
    LINES=${2:-100}
    $SSH_CMD "tail -n $LINES /tmp/plxnative-events.log" 2>/dev/null
else
    LINES=${1:-100}
    $SSH_CMD "tail -n $LINES /tmp/plxnative-events.log | grep -v 'key type='" 2>/dev/null
fi

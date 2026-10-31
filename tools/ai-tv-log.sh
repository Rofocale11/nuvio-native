#!/bin/bash
TV_IP=$(cat .tv-host)
if [ -z "$TV_IP" ]; then
    echo "Error: .tv-host is empty"
    exit 1
fi

# The base SSH command with key and silenced warnings
SSH_CMD="ssh -i ~/.ssh/id_ed25519 root@$TV_IP"

if [ "$1" == "grep" ]; then
    # Unfiltered search (searches EVERYTHING, including keys)
    $SSH_CMD "grep -a -E '$2' /tmp/plxnative-events.log | tail -n 200" 2>/dev/null

elif [ "$1" == "perf" ]; then
    # Only show frame rates, poster cache issues, and layout changes
    $SSH_CMD "grep -a -E 'loop=|posters:|imgcache:|coldopen|route=' /tmp/plxnative-events.log | tail -n 200" 2>/dev/null

elif [ "$1" == "raw" ]; then
    # EXACTLY what is in the log, no filters. Good for crashes.
    LINES=${2:-100}
    $SSH_CMD "tail -n $LINES /tmp/plxnative-events.log" 2>/dev/null

else
    # Default: view last X lines, but hide the massive hex key press spam
    LINES=${1:-100}
    $SSH_CMD "tail -n $LINES /tmp/plxnative-events.log | grep -v 'key type='" 2>/dev/null
fi
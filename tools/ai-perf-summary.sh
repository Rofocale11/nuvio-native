#!/usr/bin/env bash
# Perf-log summarizer: pulls TV perf logs and prints a one-paragraph summary.
# Usage: ./tools/ai-perf-summary.sh
set -u

TMP="$(mktemp)"
trap 'rm -f "$TMP"' EXIT

if ! ./tools/ai-tv-log.sh perf > "$TMP" 2>/dev/null; then
    echo "Failed to fetch perf logs from TV."
    exit 1
fi
if [ ! -s "$TMP" ]; then
    echo "No perf data returned from TV."
    exit 1
fi

awk '
{
    fps=""; vgap=""; carried=""; dropped=""; route=""
    for (i=1;i<=NF;i++){
        split($i,a,"=")
        if (a[1]=="fps") fps=a[2]
        else if (a[1]=="vgap") { vgap=a[2]; gsub(/ms/,"",vgap) }
        else if (a[1]=="carried") carried=a[2]
        else if (a[1]=="dropped") dropped=a[2]
        else if (a[1]=="route") route=a[2]
    }
    if (fps != "") {
        if (minfps == "" || fps+0 < minfps+0) minfps = fps+0
    }
    if (vgap != "") {
        if (maxvgap == "" || vgap+0 > maxvgap+0) maxvgap = vgap+0
    }
    if (carried != "") totalcarried += carried+0
    if (dropped != "") totaldropped += dropped+0
    if (route != "") {
        if (!(route in rmin) || fps+0 < rmin[route]+0) rmin[route] = fps+0
        rdrops[route] += (dropped=="" ? 0 : dropped+0)
        rcount[route]++
    }
}
END {
    worst=""; worstmin=999999
    for (r in rmin) {
        if (rmin[r]+0 < worstmin) { worstmin=rmin[r]+0; worst=r }
    }
    printf "Perf: min FPS %s, %d carried frames, %d dropped frames, highest vgap %s ms; worst route %s (min fps %s, %d drops) across %d samples.\n", \
        (minfps==""?"n/a":minfps), totalcarried+0, totaldropped+0, (maxvgap==""?"n/a":maxvgap), \
        (worst==""?"n/a":worst), (worst==""?"n/a":worstmin), (worst==""?0:rdrops[worst]+0), NR
}
' "$TMP"
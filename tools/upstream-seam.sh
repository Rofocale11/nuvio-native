#!/usr/bin/env bash
#
# upstream-seam.sh — where this fork actually differs from PlxNative, and what a merge would cost.
#
# WHY THIS EXISTS
# ---------------
# NuvioNative replaced PlxNative's data layer: the UI and player still talk the Plex dialect, but
# they talk it to an in-process shim (plx_pmsshim) over plx_stremio instead of to a PMS on the LAN.
# That is a deliberate, structural divergence, and it means the origin project is NOT a branch this
# fork can merge wholesale. The policy that follows from that lives in docs/nuvio/UPSTREAM.md.
#
# This fork has THREE lineages, and the default ref is the middle one, not `upstream/main`:
#
#   GLinnik21/plx-native  ->  RichiL10/plx-native feature/v0.9-hq  ->  this fork
#   (origin: UI, GPU,         (HQ fork: the in-app AC3 encode lane,      (DTS->AC3, plus the
#    Starfish/ACB, devices)    DV P7->8.1, routing, subtitles)           Nuvio/Stremio shim)
#
# `feature/v0.9-hq` CONTAINS `upstream/main` — origin's main and the v0.9.0 release are ancestors
# of it — so it is a superset of both lineages, and measuring against `upstream/main` understates
# what is available (268 commits there, 360 on the HQ branch). That is why the default is the HQ
# branch; `--ref upstream/main` still works, and is a good way to see why it is the wrong one.
#
# The problem this script solves is smaller but it is the one that actually bites: nobody knows
# WHICH files are the seam. Without that list, every upstream fix is a coin flip — is it a
# one-file cherry-pick, or does it land in a file this fork has rewritten? Guessing costs a
# session each time, and guessing wrong costs a broken build.
#
# So the seam is computed, never remembered. Nothing here is a list of files to keep in sync by
# hand; if a file stops diverging it drops out, and if one starts it appears.
#
# WHY THE BASE IS COMPUTED AND NOT PINNED
# ---------------------------------------
# AGENTS.md used to name `4e454ea` as the fork's base. It is not: that commit is an UPSTREAM commit
# dated 2026-10-11 that this fork's history does not contain, so diffing against it silently
# reports upstream's own later work as though it were ours. The only defensible base is the
# merge-base with the upstream ref, which git computes. `check` compares that against the base
# recorded in docs/nuvio/UPSTREAM.md and says when the two disagree — which is the moment someone
# needs to decide whether the fork just moved or the document went stale.
#
# SUBCOMMANDS
# -----------
#   drift       (default) how far each side has moved, and the base each is measured from.
#   diff        the files this fork has changed since the base, grouped by crate, with churn.
#               This is the seam: the files an upstream fix can collide with.
#   contested   files changed on BOTH sides since the base. This is the conflict surface — the
#               set a merge (or a cherry-pick that touches a neighbouring change) has to resolve.
#               Answers "how bad is this fix going to be", which is the only question that matters
#               when deciding whether to take an upstream commit.
#   check       exit 1 when the computed base and the documented base disagree. Safe to run by
#               hand; deliberately NOT wired into `make check` or CI.
#
# USAGE
#   tools/upstream-seam.sh [drift|diff|contested|check] [--limit N] [--base <rev>] [--ref <rev>]
#
# Environment:
#   UPSTREAM_REF   ref to measure against (default: upstream/feature/v0.9-hq, the HQ line)
#
# The upstream remote must already be fetched; this script never touches the network, because a
# fetch that quietly fails is how a stale base gets mistaken for a stable one. Run
# `git fetch upstream` yourself first, and read the `drift` line before trusting anything else.
#
set -euo pipefail

UPSTREAM_REF="${UPSTREAM_REF:-upstream/feature/v0.9-hq}"
DOC="docs/nuvio/UPSTREAM.md"
LIMIT=15
BASE_OVERRIDE=""

usage() {
    cat <<'EOF'
upstream-seam.sh — where this fork differs from PlxNative, and what a merge would cost.

usage: tools/upstream-seam.sh [drift|diff|contested|check] [--limit N] [--base <rev>] [--ref <rev>]

  drift      (default) how far each side has moved, from which base.
  diff       files this fork changed since the base, by crate, with churn.
  contested  files changed on BOTH sides since the base: the conflict surface.
  check      exit 1 if the base recorded in docs/nuvio/UPSTREAM.md is not the computed one.

Env: UPSTREAM_REF (default upstream/feature/v0.9-hq, the HQ line). --ref overrides it.
The ref must already be fetched; this never touches the network, because a fetch that
quietly fails looks exactly like a stable base.
EOF
    exit "${1:-0}"
}

CMD=""
while [ $# -gt 0 ]; do
    case "$1" in
        drift|diff|contested|check) CMD="$1"; shift ;;
        --limit) LIMIT="$2"; shift 2 ;;
        --base) BASE_OVERRIDE="$2"; shift 2 ;;
        --ref) UPSTREAM_REF="$2"; shift 2 ;;
        -h|--help) usage 0 ;;
        *) echo "upstream-seam: unknown argument '$1'" >&2; usage 1 ;;
    esac
done
CMD="${CMD:-drift}"

command -v git >/dev/null || { echo "upstream-seam: git not found" >&2; exit 1; }
git rev-parse --git-dir >/dev/null 2>&1 || { echo "upstream-seam: not inside a git work tree" >&2; exit 1; }

if ! git rev-parse --verify --quiet "$UPSTREAM_REF^{commit}" >/dev/null; then
    echo "upstream-seam: '$UPSTREAM_REF' is not resolvable." >&2
    echo "               Fetch it first:  git fetch upstream" >&2
    echo "               (a missing ref is not an empty diff — it must not read as 'no divergence')" >&2
    exit 1
fi

HEAD_REV="$(git rev-parse --verify HEAD)"
if [ -n "$BASE_OVERRIDE" ]; then
    BASE="$(git rev-parse --verify "$BASE_OVERRIDE^{commit}")" || {
        echo "upstream-seam: --base '$BASE_OVERRIDE' does not resolve to a commit" >&2
        exit 1
    }
else
    BASE="$(git merge-base HEAD "$UPSTREAM_REF")" || {
        echo "upstream-seam: no merge base with '$UPSTREAM_REF' (unrelated histories?)" >&2
        exit 1
    }
fi

# The base recorded in the doc, if the doc has one. It is advisory: `drift` reports it and `check`
# treats disagreement as a failure, but every other subcommand keeps working from the computed one.
documented_ref() {
    [ -f "$DOC" ] || return 0
    sed -n 's/.*upstream-ref: \([^ ]*[^ ]\).*/\1/p' "$DOC" | head -n 1
}

documented_base() {
    [ -f "$DOC" ] || return 0
    # The marker is an HTML comment so it renders as nothing while staying greppable.
    sed -n 's/.*upstream-base: \([0-9a-f]\{40\}\).*/\1/p' "$DOC" | head -n 1
}

commit_label() { git log -1 --format='%h  %ci  %s' "$1"; }

churn_line() { # <numstat-stdin> -> "N files  +A -D"
    awk -F'\t' '{a+=$1; d+=$2; n++} END{printf "%d file(s)  +%d -%d\n", n, a, d}'
}

# tag_of <path>: labels the files whose churn is NOT hand-written source, so a table sorted by
# insertions cannot make a vendored bundle look like the fork's largest edit. `js-service/server.js`
# alone is ~102k lines of vendored Stremio server and would otherwise dwarf every real change.
tag_of() {
    case "$1" in
        js-service/*|pkg/*|site/*) printf 'vendored' ;;
        tests/fixtures/*|tests/demo_library/*) printf 'recorded' ;;
        locales/*) printf 'translated' ;;
        docs/*|*.md) printf 'prose' ;;
        *) printf '' ;;
    esac
}

cmd_drift() {
    local doc_base upstream_ahead ours_ahead common
    doc_base="$(documented_base)"
    upstream_ahead="$(git rev-list --count "$HEAD_REV..$UPSTREAM_REF")"
    ours_ahead="$(git rev-list --count "$UPSTREAM_REF..$HEAD_REV")"

    echo "upstream ref : $UPSTREAM_REF  ($(git rev-parse --short "$UPSTREAM_REF"))"
    echo "computed base: $BASE"
    echo "               $(commit_label "$BASE")"
    if [ -n "$doc_base" ]; then
        echo "documented   : ${doc_base:0:9}"
        if [ "$doc_base" != "$BASE" ]; then
            echo
            echo "  DRIFT: the documented base is not the computed one."
            if git cat-file -e "$doc_base^{commit}" 2>/dev/null; then
                echo "         documented: $(commit_label "$doc_base")"
            else
                echo "         documented: (not a commit in this repository)"
            fi
            echo "         Decide whether the fork moved or $DOC is stale, then update the marker."
        fi
    else
        echo "documented   : (none recorded in $DOC)"
    fi
    echo
    echo "commits since base: ours $ours_ahead   upstream $upstream_ahead   (base..ours / base..upstream)"
    echo "files changed     : ours $(git diff --name-only "$BASE" "$HEAD_REV" | wc -l | tr -d ' ')   upstream $(git diff --name-only "$BASE" "$UPSTREAM_REF" | wc -l | tr -d ' ')"
    echo "contested         : $(contested_paths | wc -l | tr -d ' ') file(s) changed on both sides"
}

# contested_paths: files changed by both sides since the base. Shared by drift/contested so the
# number in `drift` and the list in `contested` can never disagree.
contested_paths() {
    local tmp_ours tmp_up
    tmp_ours="$(mktemp)"; tmp_up="$(mktemp)"
    # shellcheck disable=SC2064
    trap "rm -f '$tmp_ours' '$tmp_up'" RETURN
    git diff --name-only "$BASE" "$HEAD_REV" | LC_ALL=C sort > "$tmp_ours"
    git diff --name-only "$BASE" "$UPSTREAM_REF" | LC_ALL=C sort > "$tmp_up"
    LC_ALL=C comm -12 "$tmp_ours" "$tmp_up"
}

cmd_diff() {
    local tmp
    tmp="$(mktemp)"
    # shellcheck disable=SC2064
    trap "rm -f '$tmp'" RETURN
    git diff --numstat "$BASE" "$HEAD_REV" > "$tmp"

    echo "seam vs $BASE ($(git rev-parse --short "$BASE")) — files this fork changed, by crate"
    echo

    # Two passes over the same numstat: per-crate totals, then the biggest single files.
    awk -F'\t' -v OFS='' '{
        split($3, p, "/")
        k = ($3 ~ /^rust-modules\/[^\/]+\//) ? p[1] "/" p[2] :
            ($3 ~ /^rust-modules\/Cargo\./) ? "rust-modules/Cargo" :
            ($3 ~ /^tests\//) ? "tests" :
            ($3 ~ /^docs\//) ? "docs" :
            ($3 ~ /^\.github\//) ? ".github" : p[1]
        a[k] += $1; d[k] += $2; n[k]++
    } END {
        for (k in a) printf "%s\t%d\t%d\t%d\n", k, n[k], a[k], d[k]
    }' "$tmp" | sort -t$'\t' -k3,3nr | awk -F'\t' '{
        printf "  %-26s %4d file(s)  +%-7d -%d\n", $1, $2, $3, $4
    }'
    echo
    # Top files by insertions, each tagged when its churn is not hand-written source. A tag is
    # printed for the reader, not used to filter: the totals above stay honest either way.
    git diff --numstat "$BASE" "$HEAD_REV" | awk -F'\t' '$1 != "-"' \
        | sort -t$'\t' -k1,1nr | head -n "$LIMIT" \
        | while IFS=$'\t' read -r add del path; do
            tag="$(tag_of "$path")"
            if [ -n "$tag" ]; then
                printf '  %-6s %-6s %-58s (%s)\n' "+$add" "-$del" "$path" "$tag"
            else
                printf '  %-6s %-6s %s\n' "+$add" "-$del" "$path"
            fi
        done
    echo
    # The number a merge question actually turns on: how much UPSTREAM-OWNED SOURCE this fork
    # edited. New files inside rust-modules/ are the fork's own crates and are listed separately,
    # because they cannot conflict — they simply did not exist upstream.
    local src_modified src_added
    src_modified="$(git diff --name-only --diff-filter=M "$BASE" "$HEAD_REV" -- rust-modules src Makefile Cargo.toml Cargo.lock ipkroot 2>/dev/null | wc -l | tr -d ' ')"
    src_added="$(git diff --name-only --diff-filter=A "$BASE" "$HEAD_REV" -- rust-modules src Makefile Cargo.toml Cargo.lock ipkroot 2>/dev/null | wc -l | tr -d ' ')"
    echo "  upstream-owned source: $src_modified file(s) EDITED, $src_added file(s) added"
    echo "  (edited files are where an upstream change can collide; added ones cannot conflict)"
}

cmd_contested() {
    local tmp_paths tmp_num
    tmp_paths="$(mktemp)"; tmp_num="$(mktemp)"
    # shellcheck disable=SC2064
    trap "rm -f '$tmp_paths' '$tmp_num'" RETURN
    contested_paths > "$tmp_paths"
    git diff --numstat "$BASE" "$HEAD_REV" > "$tmp_num"

    local total
    total="$(wc -l < "$tmp_paths" | tr -d ' ')"
    echo "contested files vs $UPSTREAM_REF — changed on BOTH sides since $(git rev-parse --short "$BASE")"
    echo "total: $total"
    echo
    # Sort the contested set by OUR churn, because a file this fork barely touched is a cheap
    # cherry-pick and a file it rewrote is where the time goes. That ordering is the whole point.
    awk -F'\t' 'FNR == NR { c[$1] = 1; next } ($3 in c) { printf "%s\t%s\t%s\n", $1, $2, $3 }' \
        "$tmp_paths" "$tmp_num" | sort -t$'\t' -k1,1nr | head -n "$LIMIT" \
        | while IFS=$'\t' read -r add del path; do
            tag="$(tag_of "$path")"
            if [ -n "$tag" ]; then
                printf '  %-6s %-6s %-52s (%s)\n' "+$add" "-$del" "$path" "$tag"
            else
                printf '  %-6s %-6s %s\n' "+$add" "-$del" "$path"
            fi
        done
    echo
    echo "  Top $LIMIT by OUR churn. Files absent from this list are upstream-only changes and"
    echo "  are the cheap cherry-picks; anything here needs the seam read first."
}

cmd_check() {
    local doc_base doc_ref
    doc_base="$(documented_base)"
    doc_ref="$(documented_ref)"
    if [ -z "$doc_base" ]; then
        echo "upstream-seam: $DOC records no base marker"
        exit 1
    fi
    # The ref is checked as well as the base, because a wrong ref is the failure this file exists
    # to prevent: `upstream/main` is a SUBSET of the HQ branch, so measuring against it looks like
    # a smaller, cheaper upstream than the one this fork actually tracks.
    if [ -n "$doc_ref" ] && [ "$doc_ref" != "$UPSTREAM_REF" ]; then
        echo "upstream-seam: reference branch drift"
        echo "  documented: $doc_ref"
        echo "  in use    : $UPSTREAM_REF"
        exit 1
    fi
    if [ "$doc_base" != "$BASE" ]; then
        echo "upstream-seam: base drift"
        echo "  documented: $doc_base"
        echo "  computed  : $BASE"
        exit 1
    fi
    echo "upstream-seam: matches $UPSTREAM_REF ($(git rev-parse --short "$BASE"))"
}

case "$CMD" in
    drift) cmd_drift ;;
    diff) cmd_diff ;;
    contested) cmd_contested ;;
    check) cmd_check ;;
esac

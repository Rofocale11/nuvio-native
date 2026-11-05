#!/usr/bin/env bash
#
# ai-docs-refresh.sh — regenerate the derived numbers that AGENTS.md and
# docs/nuvio/UPSTREAM.md quote about the branch topology.
#
# WHY THIS EXISTS
# ---------------
# Those docs carry counts that move on every commit: how far the working line is ahead of the
# trunk, how many commits/files/contested paths sit on each side of the upstream seam. Hand-
# editing them is how "three commits ahead" outlived the truth by sixteen commits. The numbers
# are now generated; the prose around them is not.
#
# USAGE
#   tools/ai-docs-refresh.sh                        # update: rewrite every marked number in place
#   tools/ai-docs-refresh.sh update --trunk-follows # ...for a commit the trunk will be ff'd to as well
#   tools/ai-docs-refresh.sh check                  # read-only: exit 1 if any marked number is stale
#
# `check` is wired into ci/check-python-steps.txt, so `make check` and CI's host-python job (when
# that job runs) execute it. A docs-only push starts no CI by design. Run the update form after
# any commit that moves the working line, the trunk, or the reference branch. Never hand-edit a
# value between markers.
#
# MARKERS
#   <!-- docs-fresh:NAME -->value<!-- /docs-fresh:NAME -->          (inline)
#   <!-- BEGIN docs-fresh:NAME -->\n...\n<!-- END docs-fresh:NAME -->  (a whole table)
#
# Ref resolution: the local branch first, then origin/<branch>, so this works in a plain
# checkout and in CI. The upstream reference is optional: without it the seam tables are left
# alone and `check` only grades the branch count. Run `git fetch upstream` for the full check.
#
set -euo pipefail

MODE="${1:-update}"
TRUNK_FOLLOWS=0
case "$MODE" in
    update|check) ;;
    *) echo "usage: $0 [update|check] [--trunk-follows]" >&2; exit 2 ;;
esac
shift || true
while [ $# -gt 0 ]; do
    case "$1" in
        --trunk-follows) TRUNK_FOLLOWS=1; shift ;;
        *) echo "usage: $0 [update|check] [--trunk-follows]" >&2; exit 2 ;;
    esac
done
if [ "$MODE" = check ] && [ "$TRUNK_FOLLOWS" = 1 ]; then
    echo "ai-docs-refresh: --trunk-follows describes the commit about to be made, not a check" >&2
    exit 2
fi

export PATH="$HOME/.cargo/bin:$PATH"
# Prefer the enclosing work tree; fall back to this script's own checkout so an agent can call it
# by absolute path from any directory (the tools/ directory is committed in the repo root).
if ! ROOT="$(git rev-parse --show-toplevel 2>/dev/null)"; then
    ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
fi
cd "$ROOT"
if ! git rev-parse --git-dir >/dev/null 2>&1; then
    echo "ai-docs-refresh: not inside a git work tree ($ROOT)" >&2
    exit 2
fi

resolve() {
    local r
    for r in "$@"; do
        if git rev-parse --verify --quiet "$r^{commit}" >/dev/null; then
            printf '%s\n' "$r"
            return 0
        fi
    done
    return 1
}

TRUNK="$(resolve nuvio-native origin/nuvio-native || true)"
WORK="$(resolve forward-port/v0.9-hq origin/forward-port/v0.9-hq || true)"
REF="$(resolve upstream/feature/v0.9-hq || true)"

if [ -z "$TRUNK" ] || [ -z "$WORK" ]; then
    echo "ai-docs-refresh: cannot see the trunk and the working branch" >&2
    echo "                 (tried nuvio-native/origin/nuvio-native and" >&2
    echo "                  forward-port/v0.9-hq/origin/forward-port/v0.9-hq)" >&2
    # A docs-only CI checkout may lack one of them; do not turn that into a red build.
    [ "$MODE" = check ] && exit 0
    exit 1
fi

AHEAD="$(git rev-list --count "$TRUNK..$WORK")"
BEHIND="$(git rev-list --count "$WORK..$TRUNK")"
# The commit that carries these numbers lands on whichever branch is checked out. Made on the
# working line it is a commit the trunk does not have yet, so a `check` on the COMMITTED tree
# counts it and a number written from HEAD alone is always one short (that is how 788b7bb7 landed
# saying 41 against a real 42). Made on the trunk — the state after the trunk was fast-forwarded
# to the working line — it never reaches the working line and neither count moves, so adding one
# would make `update` and `check` disagree forever. `update` therefore writes the count the tree
# will have after that commit; `check` keeps grading HEAD, which is what a committed tree is.
DOCS_ON_WORK=0
if [ "$MODE" = update ] && [ "$(git rev-parse --abbrev-ref HEAD 2>/dev/null)" = "$WORK" ]; then
    DOCS_ON_WORK=1
    AHEAD=$((AHEAD + 1))
fi
# `--trunk-follows`: the commit is made on the working line and the trunk is fast-forwarded to it
# immediately afterwards. Both branches then point at the same commit, so the ahead count of the
# committed tree is 0 even though our side of the seam gains the commit.
if [ "$TRUNK_FOLLOWS" = 1 ]; then
    AHEAD=0
fi
HAVE_REF=0
BASE="" OURS="" REF_AHEAD="" FILES_OURS="" FILES_REF="" CONTESTED="" CONTESTED_SM=""
EDITED="" ADDED="" BASE_SHA="" BASE_DATE="" BASE_SUBJECT="" REF_SHA="" REF_DATE=""

if [ -n "$REF" ]; then
    HAVE_REF=1
    # The seam base is the trunk's merge-base with the reference. Do NOT pass WORK as a third
    # argument: `git merge-base A B C` answers with the merge-base of A and B, which here is the
    # trunk itself, and that silently reports only the working line's own commits as the seam.
    BASE="$(git merge-base "$TRUNK" "$REF")"
    OURS="$(git rev-list --count "$BASE..$WORK")"
    # Same off-by-one as AHEAD, and the same caveat: a commit made on the working line is on our
    # side of the seam, and so is one the trunk will be fast-forwarded to.
    if [ "$DOCS_ON_WORK" = 1 ] || [ "$TRUNK_FOLLOWS" = 1 ]; then OURS=$((OURS + 1)); fi
    REF_AHEAD="$(git rev-list --count "$BASE..$REF")"
    FILES_OURS="$(git diff --name-only "$BASE" "$WORK" | wc -l | tr -d ' ')"
    FILES_REF="$(git diff --name-only "$BASE" "$REF" | wc -l | tr -d ' ')"
    CONTESTED_LIST="$(comm -12 \
        <(git diff --name-only "$BASE" "$WORK" | LC_ALL=C sort) \
        <(git diff --name-only "$BASE" "$REF" | LC_ALL=C sort))"
    CONTESTED="$(printf '%s\n' "$CONTESTED_LIST" | grep -c . || true)"
    CONTESTED_SM="$(printf '%s\n' "$CONTESTED_LIST" | grep -Ec '^rust-modules/(screens|media)/' || true)"
    EDITED="$(git diff --name-only --diff-filter=M "$BASE" "$WORK" -- rust-modules src Makefile Cargo.toml Cargo.lock ipkroot | wc -l | tr -d ' ')"
    ADDED="$(git diff --name-only --diff-filter=A "$BASE" "$WORK" -- rust-modules src Makefile Cargo.toml Cargo.lock ipkroot | wc -l | tr -d ' ')"
    BASE_SHA="$(git rev-parse --short "$BASE")"
    BASE_DATE="$(git log -1 --date=short --format=%ad "$BASE")"
    BASE_SUBJECT="$(git log -1 --format=%s "$BASE" | sed 's/|/\\|/g')"
    REF_SHA="$(git rev-parse --short "$REF")"
    REF_DATE="$(git log -1 --date=short --format=%ad "$REF")"
else
    echo "ai-docs-refresh: '$REF' is not resolvable; skipping the seam tables" >&2
    echo "                 (run: git fetch upstream)" >&2
fi

export MODE AHEAD BEHIND HAVE_REF
export BASE OURS REF_AHEAD FILES_OURS FILES_REF CONTESTED CONTESTED_SM
export EDITED ADDED BASE_SHA BASE_DATE BASE_SUBJECT REF_SHA REF_DATE

python3 - <<'PY'
import os
import re
import sys

mode = os.environ["MODE"]
have_ref = os.environ["HAVE_REF"] == "1"

values = {"ahead": os.environ["AHEAD"], "behind": os.environ["BEHIND"]}
if have_ref:
    values["contested"] = os.environ["CONTESTED"]
    values["contested-screens"] = os.environ["CONTESTED_SM"]
    values["edited"] = os.environ["EDITED"]

def base_table():
    return (
        "| | |\n"
        "|---|---|\n"
        f"| reference | `upstream/feature/v0.9-hq` (`{os.environ['REF_SHA']}`, {os.environ['REF_DATE']}) |\n"
        f"| merge-base with `nuvio-native` and `forward-port/v0.9-hq` | `{os.environ['BASE_SHA']}` — {os.environ['BASE_SUBJECT']}, {os.environ['BASE_DATE']} |\n"
        f"| commits on our side since the base | {os.environ['OURS']} |\n"
        f"| commits on the reference side since the base | {os.environ['REF_AHEAD']} |\n"
        "\n"
        "(Generated by `tools/ai-docs-refresh.sh`; `drift` is the authority when this drifts.)\n"
    )

def cost_table():
    return (
        "| | |\n"
        "|---|---|\n"
        f"| files this fork changed | {os.environ['FILES_OURS']} |\n"
        f"| files the reference branch changed | {os.environ['FILES_REF']} |\n"
        f"| **contested** — changed on **both** sides | {os.environ['CONTESTED']} |\n"
        f"| of those, under `rust-modules/screens` or `rust-modules/media` | {os.environ['CONTESTED_SM']} |\n"
        f"| upstream-owned source **edited** (`rust-modules/`, `src/`, `Makefile`, `Cargo.*`, `ipkroot/`) | {os.environ['EDITED']} |\n"
        f"| upstream-owned source **added** (the fork's own crates) | {os.environ['ADDED']} |\n"
    )

blocks = {}
if have_ref:
    blocks = {"base-stats": base_table, "cost-stats": cost_table}

INLINE = re.compile(r"(<!-- docs-fresh:([a-z-]+) -->)(.*?)(<!-- /docs-fresh:\2 -->)", re.S)
BLOCK = re.compile(r"(<!-- BEGIN docs-fresh:([a-z-]+) -->\n)(.*?)(<!-- END docs-fresh:\2 -->)", re.S)

def fill_block(match):
    name = match.group(2)
    if name not in blocks:
        return match.group(0)
    return match.group(1) + blocks[name]() + match.group(4)

def fill_inline(match):
    name = match.group(2)
    if name not in values:
        return match.group(0)
    return match.group(1) + values[name] + match.group(4)

paths = ("AGENTS.md", "docs/nuvio/UPSTREAM.md")
texts = {}
for path in paths:
    with open(path, encoding="utf-8") as f:
        texts[path] = f.read()

# A check that silently grades nothing is worse than no check: fail when a marker was removed.
if "<!-- docs-fresh:ahead -->" not in "".join(texts.values()):
    print("ai-docs-refresh: no <!-- docs-fresh:ahead --> marker found")
    sys.exit(1)
if have_ref:
    for name in blocks:
        begin = f"<!-- BEGIN docs-fresh:{name} -->"
        end = f"<!-- END docs-fresh:{name} -->"
        if begin not in texts["docs/nuvio/UPSTREAM.md"] or end not in texts["docs/nuvio/UPSTREAM.md"]:
            print(f"ai-docs-refresh: missing block markers for {name!r} in docs/nuvio/UPSTREAM.md")
            sys.exit(1)

stale = []
updated = []
for path in paths:
    before = texts[path]
    after = BLOCK.sub(fill_block, before)
    after = INLINE.sub(fill_inline, after)
    if after == before:
        continue
    if mode == "update":
        with open(path, "w", encoding="utf-8") as f:
            f.write(after)
        updated.append(path)
    else:
        stale.append(path)

if mode == "check":
    if stale:
        print("ai-docs-refresh: stale derived numbers in " + ", ".join(stale))
        print("  run: ./tools/ai-docs-refresh.sh update")
        sys.exit(1)
    print("ai-docs-refresh: derived numbers are current")
else:
    if updated:
        print("ai-docs-refresh: updated " + ", ".join(updated))
    else:
        print("ai-docs-refresh: nothing to update")
PY

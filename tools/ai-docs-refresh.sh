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
#   tools/ai-docs-refresh.sh                         # update: rewrite every marked number in place
#   tools/ai-docs-refresh.sh update --trunk-follows  # ...for a commit the trunk will be ff'd to as well
#   tools/ai-docs-refresh.sh update --amend          # ...for `git commit --amend` (replaces HEAD, adds none)
#   tools/ai-docs-refresh.sh check                   # read-only: exit 1 if any marked number is stale
#
# **A commit does not need you to run this.** `.githooks/prepare-commit-msg` runs the update form
# and stages the two files with every commit, so ONE commit carries both the change and the numbers
# it implies. It works because `update` writes the count the tree will have AFTER the commit while
# `check` grades HEAD — which is why running the update form by hand before a commit used to need a
# second commit, and why a forgotten one left `make check` red and the prose a commit stale.
# Install it once per clone with `make hooks`.
#
# `check` is wired into ci/check-python-steps.txt, so `make check` and CI's host-python job (when
# that job runs) execute it. A docs-only push starts no CI by design. Never hand-edit a value
# between markers.
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
AMEND=0
USAGE="usage: $0 [update|check] [--trunk-follows] [--amend]"
case "$MODE" in
    update|check) ;;
    *) echo "$USAGE" >&2; exit 2 ;;
esac
shift || true
while [ $# -gt 0 ]; do
    case "$1" in
        --trunk-follows) TRUNK_FOLLOWS=1; shift ;;
        --amend) AMEND=1; shift ;;
        *) echo "$USAGE" >&2; exit 2 ;;
    esac
done
if [ "$MODE" = check ] && [ $((TRUNK_FOLLOWS + AMEND)) -gt 0 ]; then
    echo "ai-docs-refresh: --trunk-follows/--amend describe the commit about to be made, not a check" >&2
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
# **Two readings of the same numbers, and both have to be legal.**
#
# `HEAD` is the committed state: what CI grades, and what a clean checkout must match exactly.
# `PENDING` is the state after the commit being prepared, which `update` writes and the hook
# stages. Both differ from HEAD by exactly one commit, on whichever branch that commit extends:
# the working line adds to `ahead`, the trunk adds to `behind`. Getting this wrong in ONE
# direction is how 788b7bb7 landed saying 41 against a real 42.
#
# `--amend` replaces HEAD instead of extending it, so it bumps neither; `--trunk-follows` describes
# a commit the trunk is then fast-forwarded to, which leaves both branches on the same commit, so
# `ahead` is 0 afterwards.
HEAD_BRANCH="$(git rev-parse --abbrev-ref HEAD 2>/dev/null || true)"
AHEAD_HEAD=$AHEAD
BEHIND_HEAD=$BEHIND
AHEAD_PENDING=$AHEAD
BEHIND_PENDING=$BEHIND
if [ "$TRUNK_FOLLOWS" = 1 ]; then
    AHEAD_PENDING=0
elif [ "$AMEND" = 0 ]; then
    if [ "$HEAD_BRANCH" = "$WORK" ]; then
        AHEAD_PENDING=$((AHEAD + 1))
    elif [ "$HEAD_BRANCH" = "$TRUNK" ]; then
        BEHIND_PENDING=$((BEHIND + 1))
    fi
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
    # Same off-by-one as AHEAD, and the same two readings: a commit being prepared on the working
    # line is on our side of the seam, and so is one the trunk will be fast-forwarded to.
    OURS_HEAD=$OURS
    OURS_PENDING=$OURS
    if [ "$AHEAD_PENDING" -gt "$AHEAD_HEAD" ] || [ "$TRUNK_FOLLOWS" = 1 ]; then OURS_PENDING=$((OURS + 1)); fi
    REF_AHEAD="$(git rev-list --count "$BASE..$REF")"

    # **A table generated BEFORE a commit cannot see the paths that commit adds**, so a commit
    # carrying a new file shipped a file count one short (measured: 3c4c1f62 wrote 491 against a
    # real 492). The index can see them — the hook runs after `git add` — so every count has a HEAD
    # reading and a PENDING one, and `check` accepts whichever the tree state allows.
    REF_FILES="$(git diff --name-only "$BASE" "$REF" | LC_ALL=C sort)"
    WORK_FILES_HEAD="$(git diff --name-only "$BASE" "$WORK" | LC_ALL=C sort)"
    WORK_FILES_PENDING="$({ printf '%s\n' "$WORK_FILES_HEAD"; \
        git diff --cached --name-only --diff-filter=ACMR; } | LC_ALL=C sort -u | sed '/^$/d')"
    count_lines() { printf '%s\n' "$1" | grep -c . || true; }
    contested_with_ref() { comm -12 <(printf '%s\n' "$1") <(printf '%s\n' "$REF_FILES") || true; }

    FILES_REF="$(count_lines "$REF_FILES")"
    FILES_OURS_HEAD="$(count_lines "$WORK_FILES_HEAD")"
    FILES_OURS_PENDING="$(count_lines "$WORK_FILES_PENDING")"
    CONTESTED_HEAD="$(count_lines "$(contested_with_ref "$WORK_FILES_HEAD")")"
    CONTESTED_PENDING="$(count_lines "$(contested_with_ref "$WORK_FILES_PENDING")")"
    CONTESTED_SM_HEAD="$(contested_with_ref "$WORK_FILES_HEAD" | grep -Ec '^rust-modules/(screens|media)/' || true)"
    CONTESTED_SM_PENDING="$(contested_with_ref "$WORK_FILES_PENDING" | grep -Ec '^rust-modules/(screens|media)/' || true)"

    # `edited`/`added` count statuses, so they cannot be unioned by name alone: the staged side is
    # asked with the same filter and the two lists are merged.
    ours_paths() {
        # A staged file that is ALSO modified since the base appears in both diffs, and the answer
        # is a COUNT of files, so the two sides are unioned here rather than concatenated: without
        # `sort -u` the commit's own hook counted 15 edited files twice (240 against a real 225).
        if [ "$2" = pending ]; then
            { git diff --name-only --diff-filter="$1" "$BASE" "$WORK" -- rust-modules src Makefile Cargo.toml Cargo.lock ipkroot
              git diff --cached --name-only --diff-filter="$1" -- rust-modules src Makefile Cargo.toml Cargo.lock ipkroot
            } | LC_ALL=C sort -u
        else
            git diff --name-only --diff-filter="$1" "$BASE" "$WORK" -- rust-modules src Makefile Cargo.toml Cargo.lock ipkroot
        fi
    }
    EDITED_HEAD="$(count_lines "$(ours_paths M head)")"
    EDITED_PENDING="$(count_lines "$(ours_paths M pending)")"
    ADDED_HEAD="$(count_lines "$(ours_paths A head)")"
    ADDED_PENDING="$(count_lines "$(ours_paths A pending)")"
    BASE_SHA="$(git rev-parse --short "$BASE")"
    BASE_DATE="$(git log -1 --date=short --format=%ad "$BASE")"
    BASE_SUBJECT="$(git log -1 --format=%s "$BASE" | sed 's/|/\\|/g')"
    REF_SHA="$(git rev-parse --short "$REF")"
    REF_DATE="$(git log -1 --date=short --format=%ad "$REF")"
else
    echo "ai-docs-refresh: '$REF' is not resolvable; skipping the seam tables" >&2
    echo "                 (run: git fetch upstream)" >&2
fi

# A `check` on a DIRTY tree has a commit to account for; a check on a clean tree is grading a
# committed state and must match HEAD exactly. Without this the window between `git commit` and
# `make check` was red by construction, which is how an agent learns to ignore it.
# `git diff HEAD` covers staged and unstaged alike — the hook stages, it does not commit.
PENDING_ALLOWED=0
if ! git diff --quiet HEAD -- AGENTS.md docs/nuvio/UPSTREAM.md 2>/dev/null; then
    PENDING_ALLOWED=1
fi

export MODE HAVE_REF PENDING_ALLOWED HEAD_BRANCH
export AHEAD_HEAD BEHIND_HEAD AHEAD_PENDING BEHIND_PENDING
export OURS_HEAD OURS_PENDING REF_AHEAD FILES_REF
export FILES_OURS_HEAD FILES_OURS_PENDING CONTESTED_HEAD CONTESTED_PENDING
export CONTESTED_SM_HEAD CONTESTED_SM_PENDING EDITED_HEAD EDITED_PENDING ADDED_HEAD ADDED_PENDING
export BASE BASE_SHA BASE_DATE BASE_SUBJECT REF_SHA REF_DATE

python3 - <<'PY'
import os
import re
import sys

mode = os.environ["MODE"]
have_ref = os.environ["HAVE_REF"] == "1"
# The two readings described in the shell half: the committed state and the state after the commit
# being prepared. `update` writes the second; `check` accepts either, but only when the docs are
# dirty (so a clean tree still has to match HEAD exactly, which is the case a stale number hides in).
vals_head = {
    "ahead": os.environ["AHEAD_HEAD"],
    "behind": os.environ["BEHIND_HEAD"],
}
vals_pending = {
    "ahead": os.environ["AHEAD_PENDING"],
    "behind": os.environ["BEHIND_PENDING"],
}
pending_allowed = os.environ["PENDING_ALLOWED"] == "1"
if have_ref:
    # Every seam number has a committed and a pending reading for the same reason (see the shell
    # half): a commit adds paths the pre-commit tree cannot show.
    for vals, suffix in ((vals_head, "HEAD"), (vals_pending, "PENDING")):
        vals.update(
            {
                "ours": os.environ["OURS_" + suffix],
                "files_ours": os.environ["FILES_OURS_" + suffix],
                "files_ref": os.environ["FILES_REF"],
                "contested": os.environ["CONTESTED_" + suffix],
                "contested-screens": os.environ["CONTESTED_SM_" + suffix],
                "edited": os.environ["EDITED_" + suffix],
                "added": os.environ["ADDED_" + suffix],
            }
        )

def base_table(v):
    return (
        "| | |\n"
        "|---|---|\n"
        f"| reference | `upstream/feature/v0.9-hq` (`{os.environ['REF_SHA']}`, {os.environ['REF_DATE']}) |\n"
        f"| merge-base with `nuvio-native` and `forward-port/v0.9-hq` | `{os.environ['BASE_SHA']}` — {os.environ['BASE_SUBJECT']}, {os.environ['BASE_DATE']} |\n"
        f"| commits on our side since the base | {v['ours']} |\n"
        f"| commits on the reference side since the base | {os.environ['REF_AHEAD']} |\n"
        "\n"
        "(Generated by `tools/ai-docs-refresh.sh`; `drift` is the authority when this drifts.)\n"
    )

def cost_table(v):
    return (
        "| | |\n"
        "|---|---|\n"
        f"| files this fork changed | {v['files_ours']} |\n"
        f"| files the reference branch changed | {v['files_ref']} |\n"
        f"| **contested** — changed on **both** sides | {v['contested']} |\n"
        f"| of those, under `rust-modules/screens` or `rust-modules/media` | {v['contested-screens']} |\n"
        f"| upstream-owned source **edited** (`rust-modules/`, `src/`, `Makefile`, `Cargo.*`, `ipkroot/`) | {v['edited']} |\n"
        f"| upstream-owned source **added** (the fork's own crates) | {v['added']} |\n"
    )

blocks = {}
if have_ref:
    blocks = {"base-stats": base_table, "cost-stats": cost_table}

INLINE = re.compile(r"(<!-- docs-fresh:([a-z-]+) -->)(.*?)(<!-- /docs-fresh:\2 -->)", re.S)
BLOCK = re.compile(r"(<!-- BEGIN docs-fresh:([a-z-]+) -->\n)(.*?)(<!-- END docs-fresh:\2 -->)", re.S)

def render(text, values):
    def fill_block(match):
        name = match.group(2)
        if name not in blocks:
            return match.group(0)
        return match.group(1) + blocks[name](values) + match.group(4)

    def fill_inline(match):
        name = match.group(2)
        if name not in values:
            return match.group(0)
        return match.group(1) + values[name] + match.group(4)

    return INLINE.sub(fill_inline, BLOCK.sub(fill_block, text))

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
pending_only = []
for path in paths:
    before = texts[path]
    after = render(before, vals_pending)
    if mode == "update":
        if after != before:
            with open(path, "w", encoding="utf-8") as f:
                f.write(after)
            updated.append(path)
        continue
    # Committed already? Then HEAD's reading is the one that must match.
    if render(before, vals_head) == before:
        continue
    # A dirty tree has a commit in flight; its numbers are the pending ones.
    if pending_allowed and after == before:
        pending_only.append(path)
        continue
    stale.append(path)

if mode == "check":
    if stale:
        print("ai-docs-refresh: stale derived numbers in " + ", ".join(stale))
        print("  run: ./tools/ai-docs-refresh.sh update")
        sys.exit(1)
    if pending_only:
        print(
            "ai-docs-refresh: numbers are current for the commit in flight in "
            + ", ".join(pending_only)
        )
    else:
        print("ai-docs-refresh: derived numbers are current")
else:
    if updated:
        print("ai-docs-refresh: updated " + ", ".join(updated))
    else:
        print("ai-docs-refresh: nothing to update")
PY

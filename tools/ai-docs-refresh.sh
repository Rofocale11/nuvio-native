#!/usr/bin/env bash
#
# ai-docs-refresh.sh — regenerate the derived numbers that AGENTS.md and
# docs/nuvio/UPSTREAM.md quote about the branch topology.
#
# WHAT THIS DOES AND WHAT IT DELIBERATELY DOES NOT
# ------------------------------------------------
# Two kinds of number live in those docs, and only one of them belongs in a commit.
#
# STORE: facts about the SHAPE of the fork — how many files it changed, how many of those the
# reference branch also touched, how many upstream-owned files it edited or added, where the seam
# sits. These move when files are added or removed. They are worth quoting, they drift slowly, and
# a wrong one is a real lie about the divergence.
#
# DO NOT STORE: how far the working line is ahead of the trunk, or how many commits sit since the
# seam. Those increment on EVERY commit, so storing them makes the docs part of the next commit's
# input: every commit has to rewrite the number the commit itself changes, every commit made
# outside the hook (a GitHub rebase merge, an amend, a bot, another worktree) leaves the tree
# stale, and the only repair is another commit — 10 of 40 consecutive commits were once nothing
# but this. Both docs now say how to ASK git instead, which cannot go stale.
#
# USAGE
#   tools/ai-docs-refresh.sh                         # update: rewrite every marked number in place
#   tools/ai-docs-refresh.sh update --pending        # ...for the commit the index is about to create
#   tools/ai-docs-refresh.sh check                   # read-only: exit 1 if any marked number is stale
#
# `update` writes the reading of the COMMITTED tree by default, so running it by hand can never
# write a number that only becomes true after a commit. Only `.githooks/pre-commit` passes
# `--pending`, because it runs on the index just before that index becomes a commit: a file the
# commit adds must be counted, and that is the only difference `--pending` makes.
# Install the hook once per clone with `make hooks`.
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
# alone and `check` grades nothing, which is not an error (see the exit-0 path below).
#
set -euo pipefail

MODE="${1:-update}"
PENDING=0
USAGE="usage: $0 [update|check] [--pending]"
case "$MODE" in
    update|check) ;;
    *) echo "$USAGE" >&2; exit 2 ;;
esac
shift || true
while [ $# -gt 0 ]; do
    case "$1" in
        --pending) PENDING=1; shift ;;
        *) echo "$USAGE" >&2; exit 2 ;;
    esac
done
if [ "$MODE" = check ] && [ "$PENDING" = 1 ]; then
    echo "ai-docs-refresh: --pending describes the commit about to be made, not a check" >&2
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

# **Two readings of the file counts, and both have to be legal.**
#
# `HEAD` is the committed state: what CI grades, and what a clean checkout must match exactly.
# `PENDING` is the same count taken over the index, for the hook: a commit that ADDS a file must
# count it, and the committed tree cannot see it yet. That is the only difference the two readings
# have — the per-commit counters (ahead/behind/commits-since-the-seam) are no longer stored at all,
# because a stored count that increments on every commit is a loop, not a measurement (see the
# header).
HAVE_REF=0
BASE="" REF_AHEAD="" FILES_OURS="" FILES_REF="" CONTESTED="" CONTESTED_SM=""
EDITED="" ADDED="" BASE_SHA="" BASE_DATE="" BASE_SUBJECT="" REF_SHA="" REF_DATE=""

if [ -n "$REF" ]; then
    HAVE_REF=1
    # The seam base is the trunk's merge-base with the reference. Do NOT pass WORK as a third
    # argument: `git merge-base A B C` answers with the merge-base of A and B, which here is the
    # trunk itself, and that silently reports only the working line's own commits as the seam.
    BASE="$(git merge-base "$TRUNK" "$REF")"
    REF_AHEAD="$(git rev-list --count "$BASE..$REF")"

    # **A table generated BEFORE a commit cannot see the paths that commit adds**, so a commit
    # carrying a new file shipped a file count one short (measured: 3c4c1f62 wrote 491 against a
    # real 492). The index can see them — the hook runs after `git add` — so every count has a HEAD
    # reading and a PENDING one, and `check` accepts whichever the tree state allows.
    REF_FILES="$(git diff --name-only "$BASE" "$REF" | LC_ALL=C sort)"
    WORK_FILES_HEAD="$(git diff --name-only "$BASE" "$WORK" | LC_ALL=C sort)"
    # The pending reading is ONE diff, base against the index — never the HEAD list unioned with
    # `--cached`. The union counted a fork-ADDED file as "edited" the moment a commit staged a
    # change to it, because `--cached` (index against HEAD) reports that file as `M` while against
    # the base it is `A`: 17822681 wrote 226 edited files against a real 225 this way. A single
    # `git diff --cached "$BASE"` sees the post-commit content and gets the statuses right.
    # No `--diff-filter` here: this set is the fork's whole changed-file list, and a file the fork
    # DELETED is still a file it changed. Filtering to A/C/M/R dropped 11 of them and shipped
    # 661 against a real 672.
    WORK_FILES_PENDING="$(git diff --cached --name-only "$BASE" | LC_ALL=C sort -u | sed '/^$/d')"
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
        # Same shape as WORK_FILES_PENDING above, and for the same reason: `--cached "$BASE"` is
        # the post-commit content with the right statuses. The old HEAD-plus-`--cached` union
        # counted a fork-added file as edited as soon as a commit touched it (edited 226 against a
        # real 225 in 17822681), because `--cached` sees `M` where the base sees `A`.
        if [ "$2" = pending ]; then
            git diff --cached --name-only --diff-filter="$1" "$BASE" -- rust-modules src Makefile Cargo.toml Cargo.lock ipkroot
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

export MODE HAVE_REF PENDING_ALLOWED PENDING
export REF_AHEAD FILES_REF
export FILES_OURS_HEAD FILES_OURS_PENDING CONTESTED_HEAD CONTESTED_PENDING
export CONTESTED_SM_HEAD CONTESTED_SM_PENDING EDITED_HEAD EDITED_PENDING ADDED_HEAD ADDED_PENDING
export BASE BASE_SHA BASE_DATE BASE_SUBJECT REF_SHA REF_DATE

python3 - <<'PY'
import os
import re
import sys

mode = os.environ["MODE"]
have_ref = os.environ["HAVE_REF"] == "1"
# The two readings described in the shell half. `update` writes HEAD unless the hook asked for
# `--pending`; `check` grades HEAD, and accepts the pending reading only on a dirty tree (where a
# staged new file is legitimately not in HEAD yet).
vals_head = {}
vals_pending = {}
pending_allowed = os.environ["PENDING_ALLOWED"] == "1"
if have_ref:
    # Every seam number has a committed and a pending reading for the same reason (see the shell
    # half): a commit adds paths the pre-commit tree cannot show.
    for vals, suffix in ((vals_head, "HEAD"), (vals_pending, "PENDING")):
        vals.update(
            {
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
        f"| commits on the reference side since the base | {os.environ['REF_AHEAD']} |\n"
        "\n"
        # There is deliberately no \"commits on our side\" row: it increments on every commit, so it
        # was the number that forced a docs commit after every work commit. Ask git instead —
        # `git rev-list --count <merge-base>..forward-port/v0.9-hq`.
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

# A check that silently grades nothing is worse than no check.
if have_ref:
    for name in blocks:
        begin = f"<!-- BEGIN docs-fresh:{name} -->"
        end = f"<!-- END docs-fresh:{name} -->"
        if begin not in texts["docs/nuvio/UPSTREAM.md"] or end not in texts["docs/nuvio/UPSTREAM.md"]:
            print(f"ai-docs-refresh: missing block markers for {name!r} in docs/nuvio/UPSTREAM.md")
            sys.exit(1)
elif mode == "check":
    print("ai-docs-refresh: no upstream reference in this checkout — nothing to grade (not an error)")

stale = []
updated = []
pending_only = []
for path in paths:
    before = texts[path]
    head = render(before, vals_head)
    pending = render(before, vals_pending)
    if mode == "update":
        target = pending if os.environ["PENDING"] == "1" else head
        if target != before:
            with open(path, "w", encoding="utf-8") as f:
                f.write(target)
            updated.append(path)
        continue
    # Committed already? Then HEAD's reading is the one that must match.
    if head == before:
        continue
    # A dirty tree has a commit in flight; its numbers are the pending ones.
    if pending_allowed and pending == before:
        pending_only.append(path)
        continue
    stale.append(path)

if mode == "check":
    if stale:
        print("ai-docs-refresh: stale derived numbers in " + ", ".join(stale))
        # Say WHICH number and in which direction, instead of sending the reader back to a command
        # that regenerates the whole file and hides the disagreement.
        import difflib

        for path in stale:
            diff = list(
                difflib.unified_diff(
                    texts[path].splitlines(),
                    render(texts[path], vals_head).splitlines(),
                    fromfile=f"{path} (what the file says)",
                    tofile=f"{path} (what git says)",
                    lineterm="",
                    n=0,
                )
            )
            for line in diff[2:14]:
                print("    " + line)
            if len(diff) > 14:
                print("    …")
        print("  fix: ./tools/ai-docs-refresh.sh update   (writes the committed reading)")
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

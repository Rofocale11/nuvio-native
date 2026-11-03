#!/usr/bin/env bash
#
# cut-release.sh — bump the package version, commit it, and tag the release.
#
# WHY THIS EXISTS
# ---------------
# A tag is only a label. The package version comes from pkg/appinfo.json,
# ipkroot/ctl/control, rust-modules/Cargo.toml and rust-modules/Cargo.lock. A tag without those
# bumped is how this fork shipped six releases whose packages all still said 0.8.0.
# `build-ipk.yml` now refuses a tag that disagrees with appinfo.json; this script produces the
# paired commit so that refusal never fires.
#
# USAGE
#   tools/cut-release.sh --current
#   tools/cut-release.sh <major|minor|patch|X.Y.Z> [--yes] [--push] [--allow-any-branch]
#
# Default is a DRY RUN: prints what would change and stops. `--yes` writes the four version
# files, verifies them with ci/check-package.py, commits `release: X.Y.Z`, and creates the
# annotated tag. `--push` also pushes the branch and tag. Nothing is pushed without --push; ask
# the owner before either --yes or --push (AGENTS.md).
#
# `build-ipk.yml` builds the tag, runs the release gates, and attaches the .ipk and its sha256 to
# a GitHub Release. This script does not create a release itself.
set -euo pipefail

export PATH="$HOME/.cargo/bin:$PATH"
if ! ROOT="$(git rev-parse --show-toplevel 2>/dev/null)"; then
    ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
fi
cd "$ROOT"
if ! git rev-parse --git-dir >/dev/null 2>&1; then
    echo "cut-release: not inside a git work tree ($ROOT)" >&2
    exit 2
fi

usage() {
    cat <<'EOF'
cut-release.sh — bump the version, commit it, and tag the release.

usage: tools/cut-release.sh <major|minor|patch|X.Y.Z> [--yes] [--push] [--allow-any-branch]
       tools/cut-release.sh --current

  (no flags)        dry run: print the version it would write and change nothing
  --yes             write the version files, verify them, commit, and tag
  --push            also push the branch and tag (requires --yes)
  --allow-any-branch  allow a branch other than forward-port/v0.9-hq or nuvio-native
  --current         print the current version and exit

build-ipk.yml builds a v* tag and attaches the .ipk + sha256 to a GitHub Release.
EOF
}

SPEC=""
YES=0
PUSH=0
ALLOW_ANY=0
CURRENT=0

while [ $# -gt 0 ]; do
    case "$1" in
        --current) CURRENT=1; shift ;;
        --yes) YES=1; shift ;;
        --push) PUSH=1; shift ;;
        --allow-any-branch) ALLOW_ANY=1; shift ;;
        -h|--help) usage; exit 0 ;;
        -*) echo "cut-release: unknown option '$1'" >&2; usage >&2; exit 2 ;;
        *)
            if [ -n "$SPEC" ]; then
                echo "cut-release: only one version spec is allowed" >&2
                exit 2
            fi
            SPEC="$1"
            shift
            ;;
    esac
done

if [ "$CURRENT" = 1 ]; then
    python3 ci/bump-version.py --current
    exit 0
fi

if [ -z "$SPEC" ]; then
    usage >&2
    exit 2
fi
case "$SPEC" in
    major|minor|patch) ;;
    [0-9]*.[0-9]*.[0-9]*) ;;
    *)
        echo "cut-release: spec must be major, minor, patch, or X.Y.Z (got '$SPEC')" >&2
        exit 2
        ;;
esac

if [ "$PUSH" = 1 ] && [ "$YES" = 0 ]; then
    echo "cut-release: --push requires --yes" >&2
    exit 2
fi

branch="$(git branch --show-current)"
if [ -z "$branch" ]; then
    echo "cut-release: detached HEAD; check out the working line first" >&2
    exit 2
fi
case "$branch" in
    forward-port/v0.9-hq|nuvio-native) ;;
    *)
        if [ "$ALLOW_ANY" = 1 ]; then
            echo "cut-release: releasing from '$branch' (not the working line or trunk)" >&2
        else
            echo "cut-release: refusing to release from '$branch'" >&2
            echo "             expected forward-port/v0.9-hq or nuvio-native;" >&2
            echo "             pass --allow-any-branch to override" >&2
            exit 1
        fi
        ;;
esac

if [ "$YES" = 1 ] && [ -n "$(git status --porcelain)" ]; then
    echo "cut-release: the working tree is not clean; commit or stash first:" >&2
    git status --short >&2
    exit 1
fi

current="$(python3 ci/bump-version.py --current)"
if [ "$YES" = 0 ]; then
    echo "branch:  $branch"
    echo "current: $current"
    echo
    echo "dry run — nothing was written. This would run:"
    echo "  python3 ci/bump-version.py $SPEC"
    python3 ci/bump-version.py "$SPEC" --dry-run
    echo
    echo "then commit the four version files, tag the result, and (only with --push) push."
    exit 0
fi

python3 ci/bump-version.py "$SPEC"
new="$(python3 ci/bump-version.py --current)"
tag="v$new"

if [ "$new" = "$current" ]; then
    echo "cut-release: the version is already $new; no bump to make" >&2
    exit 1
fi
if git rev-parse -q --verify "refs/tags/$tag" >/dev/null; then
    echo "cut-release: $tag already exists" >&2
    exit 1
fi

echo "version: $current -> $new"
python3 ci/check-package.py

echo
echo "--- version diff ---"
git diff --stat -- pkg/appinfo.json ipkroot/ctl/control rust-modules/Cargo.toml rust-modules/Cargo.lock
git diff -- pkg/appinfo.json ipkroot/ctl/control rust-modules/Cargo.toml rust-modules/Cargo.lock

git add pkg/appinfo.json ipkroot/ctl/control rust-modules/Cargo.toml rust-modules/Cargo.lock
if git diff --cached --quiet; then
    echo "cut-release: no version change staged; refusing to tag an unbumped tree" >&2
    exit 1
fi
git commit -m "release: $new"
git tag -a "$tag" -m "$new"

echo
echo "created commit and tag $tag on $branch"
if [ "$PUSH" = 1 ]; then
    git push origin "$branch" "$tag"
    echo "pushed $branch and $tag"
    echo "watch the build: gh run list --workflow build-ipk.yml --limit 3"
else
    echo "not pushed. To publish (only after the owner confirms):"
    echo "  git push origin $branch $tag"
fi

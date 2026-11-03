---
name: cut-release
description: >
  Cut and publish a release of this fork — bump the package version, commit it, tag it, and let
  build-ipk.yml build and attach the .ipk. Use when the user asks to release, ship, publish, tag,
  or bump a version ("cut 0.9.0", "ship a release", "tag a new version", "make a release"). This
  fork does not use upstream's release/rc/nightly/build-package/webosbrew train.
---

# cut-release — the version and the tag move together

## What a release is in this fork

One commit and one annotated tag:

- the commit moves the four version files: `pkg/appinfo.json`, `ipkroot/ctl/control`,
  `rust-modules/Cargo.toml`, `rust-modules/Cargo.lock`;
- the tag is `vX.Y.Z` and points at that commit;
- `build-ipk.yml` builds the tag, runs the release gates, and attaches
  `com.nuvionative.app_X.Y.Z_arm.ipk` and `ipk.sha256` to a GitHub Release.

That is the whole release path. The upstream workflows (`release.yml`, `rc.yml`, `nightly.yml`,
`build-package.yml`, `site-video.yml`, `pages.yml`, `build-bench.yml`, `ci-metrics.yml`,
`simulators.yml`) were retired in this fork and are not part of a release.

## Why the version files matter

The tag is only a label. The package version comes from `pkg/appinfo.json`, and
`ipkroot/ctl/control`, `rust-modules/Cargo.toml` and `Cargo.lock` must agree with it. A tag
without that bump is how six releases shipped packages that all still said 0.8.0.
`build-ipk.yml` now refuses a tag that disagrees with `appinfo.json`.

## Procedure

### 1. Preconditions

- `git status --short` is **clean**. Commit unrelated work first; this skill will not hide it in a
  release commit.
- You are on `forward-port/v0.9-hq` (the working line) or `nuvio-native` (the trunk). Never cut
  from `main`.
- `./tools/cut-release.sh --current` prints the tracked version.
- `./tools/ai-docs-refresh.sh check` passes when the change set touched the docs.

### 2. Pick the version

- `minor` for normal work (the fork's default), `major` for a deliberate break.
- `patch` only on a maintenance line; there is none in this fork today.
- Do not invent a number. Start from `--current`.

### 3. Dry run

```sh
./tools/cut-release.sh minor
```

It prints the old and new versions and the four files it would rewrite, and changes nothing.
Review the number; a wrong one becomes a published tag.

### 4. Ask the owner

AGENTS.md requires explicit owner confirmation before a commit or a push. Show the dry-run output
and ask. Do not proceed on a previous task's approval.

### 5. Apply

```sh
./tools/cut-release.sh minor --yes
```

This writes the four files, runs `ci/check-package.py`, commits `release: X.Y.Z`, and creates the
annotated tag. Review the result:

```sh
git show --stat HEAD
git show HEAD -- pkg/appinfo.json ipkroot/ctl/control rust-modules/Cargo.toml
```

### 6. Push, only after the owner confirms

If the owner approved commit and push together, add `--push` to the same command in step 5. If
they approved only the commit, push after reviewing:

```sh
git push origin forward-port/v0.9-hq vX.Y.Z
```

Do not re-run the script to push: it would try to bump and tag again.

### 7. Watch the build

```sh
gh run list --workflow build-ipk.yml --limit 3
gh run watch <run-id>
```

On a tag, `build-ipk` runs, in order: version agreement, tag/version guard, build, package
assertions, ELF, binary size, font hinting, webosbrew compatibility, firmware load matrix. Then it
attaches the `.ipk` and `ipk.sha256` to the GitHub Release. A manual dispatch on a branch builds
the same package but creates no release.

### 8. Verify what a user downloads

```sh
gh release view vX.Y.Z --json assets
gh release download vX.Y.Z --pattern '*.ipk' --pattern 'ipk.sha256'
```

The asset name must contain the new version, and `sha256sum` must match `ipk.sha256`. If the TV is
needed, use the `tv-lock` and `tv-session` skills, install the stable package, and confirm the
version the app reports matches the tag.

## What not to do

- Do not `git tag` or `git push --tags` by hand.
- Do not push a tag whose `pkg/appinfo.json` says something else; `build-ipk` will refuse it.
- Do not use the retired upstream release workflows, and do not publish a webosbrew manifest yet.
- Do not move or force-push a published tag. Cut the next version instead.
- Do not delete a release to "fix" a bad asset; cut the next version and explain it in the notes.

## If a step fails

- **`Tag must match the package version`**: delete the bad tag locally and on `origin`, then re-run
  the script from a clean tree.
- **`ci/check-package.py`**: the four version files disagree. Run
  `python3 ci/bump-version.py --current`, inspect the four files, and fix the stragglers before
  tagging.
- **The build is red on a gate**: fix the code, commit normally, and cut the next version. Do not
  re-tag a build users may already have downloaded.

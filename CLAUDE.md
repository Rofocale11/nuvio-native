@AGENTS.md

# Claude Code configuration

Project-specific Claude subagents are available under `.claude/agents/`. Shared skills are exposed
through `.claude/skills`, which is a relative symlink to the canonical `.agents/skills/` tree.
Claude-only hooks and the Rust analyzer plugin are configured in `.claude/settings.json`.

## Fork topology (overrides the upstream assumptions below)

`AGENTS.md` and `docs/nuvio/UPSTREAM.md` are the authority for this repository. Two inherited
assumptions are wrong here:

- **The trunk is `nuvio-native`, not `main`.** `main` holds upstream-side history only; never cut,
  build, or ship it. The working line is `forward-port/v0.9-hq`.
- **`tools/build-gc.sh --worktrees` judges lanes against `origin/main`**, so a lane cut from
  `nuvio-native` or `forward-port/v0.9-hq` always reads `unmerged` and is kept. Never delete a
  fork lane on that verdict; check `git merge-base --is-ancestor <sha> origin/nuvio-native`
  yourself. The failure is conservative (kept, not deleted), but the report is misleading.

## Worktree gotchas (Claude sessions)

- The Bash tool's "Shell cwd was reset to …" can leave you in the MAIN checkout (`/Users/…/plx-native`), not your worktree: a bare `git checkout -B x` there moves the owner's checkout off `main`. Start branch-switching commands with `cd <your worktree> &&` (or `git -C`), and confirm with `git branch --show-current`.
- A branch an `isolation: "worktree"` subagent created stays checked out in its worktree, so you cannot `git checkout` it elsewhere: `git checkout -B tmp origin/<branch>`, then `git push origin HEAD:<branch>`.
- Finished lanes: `tools/build-gc.sh --worktrees -n` previews. It judges a lane against `origin/main`, so a squash-merged lane reads removable once `git fetch` has brought the squash in; it still reads "unmerged" while `origin/main` predates the squash, or after `main` has changed that lane's lines again. A clean lane (`git status --short` empty) can be removed with plain `git worktree remove <path>` (no `--force`), which keeps its branch but takes the lane's IGNORED files too (`git status --short --ignored` lists them; `--worktrees -n` lists the ones that are not build output, e.g. `src/config.local.h`, and `--auto` keeps such a lane). The auto-mode classifier may refuse a batch removal until the owner says yes.
- The tv-lock guard hook reads the lane only from a `PLX_TV_LOCK_LANE=<worktree>` prefix at the very START of the Bash command: no `sleep;`, `cd &&` or second statement before it. Run `tv-lock.sh acquire` as its own call, then one device command per call.

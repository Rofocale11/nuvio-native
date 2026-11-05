# Agent guide — context, tools and workflow

> **Status:** current. This is the long-form companion to [`AGENTS.md`](../../AGENTS.md): the area
> map, the tool inventory and the workflow for a change that touches several layers. `AGENTS.md`
> is the short, authoritative rule list; this file explains how to apply it.

## What this repository is

Nuvio Native keeps PlxNative's GPU-rendered UI and native Starfish/ACB player, and replaces the
Plex data layer with an in-process PMS-dialect shim (`plx_pmsshim`) over Stremio addons and the
Nuvio account. There is no plex.tv credential and no real PMS. The backend design is in
[`ARCHITECTURE.md`](ARCHITECTURE.md); the running feature catalogue is [`FEATURE.md`](FEATURE.md).

Host tests and the desktop simulator cannot prove device behaviour: pixel output, LG text
rasterisation, video-plane composition, performance and native playback need the television.
`AGENTS.md` gives the rules; the `which-tier` skill picks the tier a change needs.

## Before you change an area

| area | read first |
|---|---|
| addon backend, shim dialect | [`ARCHITECTURE.md`](ARCHITECTURE.md), `rust-modules/pmsshim/`, `rust-modules/stremio/`, [`pms-api.md`](../pms-api.md) |
| Plex IDs, key table, synthetic bases | [`ID-MANAGEMENT.md`](ID-MANAGEMENT.md) |
| TMDB enrichment and metadata | [`TMDB.md`](TMDB.md) |
| playback, audio lanes, memory, ABR | [`PLAYBACK.md`](PLAYBACK.md), `rust-modules/media/src/player/CLAUDE.md` |
| detail page, prefetch, caching | [`DETAIL-LATENCY.md`](DETAIL-LATENCY.md) |
| account, My List, watch progress | [`WATCH-PROGRESS.md`](WATCH-PROGRESS.md) |
| upstream take, branch topology | [`UPSTREAM.md`](UPSTREAM.md), then [`UPSTREAM-LOG.md`](UPSTREAM-LOG.md) |
| UI widgets, layout, theme | `rust-modules/ui/src/CLAUDE.md`, [`ui-restructure-spec-v4.md`](../ui-restructure-spec-v4.md) |
| screens | `rust-modules/screens/src/CLAUDE.md` |
| build, deploy, portability, test tiers | [`agent-reference.md`](../agent-reference.md) |
| TV workflow, capture, profiling | `.agents/skills/` (`tv-lock`, `tv-session`, `ui-sim`, `profile-tv`) |

Do not load `docs/agent-reference.md` whole when one section answers the question; its headings
are the map.

## Tools

**Tools are not a menu.** The canonical entry points are the `ai-*` scripts and
`tools/upstream-seam.sh`. The skills own the rest by name (`tv-lock.sh`, `tv-session.sh`,
`screenshots.py`, `sim.ps1`, the `profile-*` tools). Everything else in `tools/` is upstream
PlxNative tooling inherited with the fork: read its header or the doc that names it before using
it. Raw `ssh` and `tools/nv-tv.py` are forbidden.

| tool | what it is for |
|---|---|
| `./tools/ai-test.sh` | nextest for the crates the uncommitted diff touches (inner loop) |
| `./tools/ai-test-hostsim.sh [filter]` | `make test-hostsim`, with a fresh runtime dir per run |
| `./tools/ai-sim.sh` | builds and launches the desktop simulator (`DISPLAY` needed) |
| `./tools/ai-tv-log.sh` | read-only device logs: `200`, `p2p`, `perf`, `panic`, `mem`, `grep`, `raw` |
| `./tools/ai-perf-summary.sh` | one-paragraph TV perf summary from the logs |
| `./tools/ai-layers.sh` | module-layer check (`ci/check-module-layers.py`) |
| `./tools/ai-docs-refresh.sh` | the derived counts; `check` is the gate form. **You do not run `update` by hand** — `.githooks/pre-commit` does it and stages both files with each commit (`git config core.hooksPath .githooks`, once per repository) |
| `./tools/upstream-seam.sh` | drift / diff / contested / check against the reference branch |
| `python3 tools/probe-addon.py …` | query Stremio addons and TMDB (`manifest`, `meta`, `streams`, `tmdb`) |

## Workflow for a change

1. Read the owning document from the table above and the rules in `AGENTS.md`.
2. Make the change at the layer that owns the behaviour — not a screen-local copy of it.
3. Add or update the test that fails without the change. Fixing a bug starts with a failing
   test or a reproducible artifact.
4. Update the owning document in the same commit. If the change makes a sentence elsewhere
   false, fix that sentence too.
5. Pick the verification tier with `which-tier`. A host-only green result is not a device result.
6. Commit. The pre-commit hook regenerates the derived counts and stages them with the change, so
   there is nothing to run by hand and no second docs-only commit. If it aborts, you have unstaged
   edits in `AGENTS.md`/`docs/nuvio/UPSTREAM.md`: `git add` them and commit again.
7. Report the `test result:` line itself, not a summary, and say which tier you actually ran.

## Orchestration

- **Subagents:** run independent searches in parallel and keep the findings, not the file dumps.
- **Search discipline:** use `rg` for text (it respects `.gitignore`); find the definition site
  before editing a Rust symbol; use a web search for current crate documentation rather than
  guessing an API.
- **GitHub:** use `gh` for PRs, CI logs and issues.
- **Files:** use the read/edit/write tools, and show the diff before applying a change when the
  harness supports it.
- **`ultrawork:` / `ulw:`** means "orchestrate this": split the independent work across
  subagents, keep the main context for decisions, and do not stop until the verification the
  change needs has passed.

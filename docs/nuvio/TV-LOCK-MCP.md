# TV-lock MCP — design only

Status: **design, no implementation**. No config was changed, no server was written, and the TV was
not touched. Written 2026-11-26. Deleting TokenEconomizer does not block this — it was never a
dependency, only a thing this direction was waiting on.

## Why

There is one physical TV, one app instance on it, and webOS enforces nothing. Two device jobs
running together do not fail cleanly: they capture each other's screens, measure each other's
binary, and write logs that look like the other lane's result.

The existing defence is enforced in the places agents drive the set from: `tools/tv-lock.sh`
(the lease, held on the device at `/tmp/plx-tv.lock`), the `tv-lock-require` Makefile prerequisite,
the scripts that inherit a lease, and the Claude Code `PreToolUse` hook
`.claude/hooks/tv-lock-guard.py`. **That hook does not run in OpenCode/OpenChamber.** An agent here
can call `shell` with `make deploy` or `tools/tv-session.sh up` and drive the set with no lease at
all. A lock-aware MCP closes that gap by putting the lease in front of the device action itself:
the agent asks the server for a device job, and the server acquires through the existing CLI, runs
one bounded job, and releases.

## What it is and is not

- It **reuses** `tools/tv-lock.sh` as the single authority — device-side lock, host timestamps,
  per-checkout mirror, pre-flight verdict. No second lock, no new lease format, no direct `ssh` or
  `scp` inside the server.
- It is **not** a general remote-control surface: only an allowlist of existing device jobs.
- It is **not** a token saving. OpenCode's permission `deny` blocks an operation; the docs do not
  promise the tool schema disappears from the request. Treat it as enforcement only.
- It is **not** dependent on TokenEconomizer (removed 2026-11-26).

## Mock PMS is not an account boundary on this fork

Upstream's `--guest --mock` boots a synthetic PMS identity. On Nuvio Native that override is
short-circuited: `nuvio::boot()` starts the in-process shim on every non-replay boot, and
`dev_token()` / `pms_origin()` / `plex_tv()` all return the shim's own token and origin
(`rust-modules/src/app/boot.rs`, `rust-modules/src/app/nuvio.rs`,
`rust-modules/src/dev/scenarios.rs`). The mock PMS's synthetic Plex titles never reach the app.

Consequences for this MCP:

- No stored Nuvio session → QR sign-in, not Home. Screenshot / `state` / `log` jobs are safe but
  capture the sign-in screen; they cannot verify Home, detail or playback.
- A stored session → Home from the shim (local watch state plus the persisted addon set), which is
  a real account context. OK on a Continue Watching card, or a hold menu opened on one, can start
  playback and write watch state. The server must not treat `--guest`/`--mock` as isolation; the
  owner chooses the account state before a TV acceptance run.
- Upstream `tests/run.py --mock` cases grade identity `1.41.0.0000-synthetic`; the shim reports
  `1.41.0.0000-nuvio`, so those cases are not a valid device gate here.
- The network-free `FakeBackend` used to be a simulator / standalone-CLI facility, not a boot mode.
  It now has one devtrig-gated boot mode shared by the simulator and the TV — see
  `docs/nuvio/SYNTHETIC-DEBUG.md`. That gives deterministic content, but it is still not an account
  boundary: the synthetic mode starts signed out and writes no account state.

This does not change the lease design; it narrows what an agent may safely claim after a run.

## Proposed tool surface

| Tool | Action | Lease |
| --- | --- | --- |
| `tv_status` | wrapper for `tv-lock.sh status`: holder, age, TTL, `IN USE:` line | none (read-only) |
| `tv_with` | one bounded job: acquire → run → release, including on error and signal | one lease per call |
| `tv_acquire`, `tv_release` | only for a run that genuinely spans several device commands | explicit, bounded |
| `tv_log`, `tv_fg` | read-only log tail / foreground read, always naming the holder | none |

`tv_with` arguments: `why` (required, recorded on the lease), `job` (allowlisted name plus its
existing arguments), `ttl` (default 30 minutes, hard maximum 45), `wait` (default 0, maximum 540
seconds per attempt, re-issued rather than held longer). It inherits a lease this lane already
holds instead of re-acquiring one. `tv_acquire` must never be used to park on the set between runs;
there is no renew between runs. If a job needs more than ~30 minutes, split it or say so in `why`.

The job allowlist maps names onto commands that already honour the lock: `deploy` (`make deploy`),
`run` (`tests/run.py …`), `capture` (`tools/capture-screen.sh`), and the `tv-session.sh` operations
`up`, `key`, `click`, `shot`, `down`. Start narrow; add jobs only with evidence of a need.

## Enforcement needs both halves

### Server half

Every device command goes through `tv-lock.sh`. Before acting, the server reads `status`, including
the pre-flight verdict. An `IN USE:` line is a **refusal**, reported verbatim as the first line of
the tool result; the only override is `PLX_TV_IN_USE_OK=1` supplied by the owner in that same
request. The server never calls `break`, never honours `PLX_TV_LOCK_BYPASS` from model input, and
never accepts a lane name from the model.

### Client half

The MCP alone is advisory if `shell` can still drive the set. Add V2 permission rules to
`~/.config/opencode/opencode.json` (global, not the repo-tracked project config, so personal host
details never enter git) denying the raw device commands:

```jsonc
{
  "permissions": [
    { "action": "shell", "resource": "ssh root@*", "effect": "deny" },
    { "action": "shell", "resource": "tools/tv-session.sh *", "effect": "deny" },
    { "action": "shell", "resource": "make deploy *", "effect": "deny" },
    { "action": "shell", "resource": "tests/run.py *", "effect": "deny" }
  ]
}
```

The full list also covers `scp`, `tools/tv-ssh`, `sshpass`, `tools/capture-screen.sh`, and the
Makefile targets `run`, `run-stream`, `kill`, `install`, `uninstall`, `verify-deploy`. Last matching
rule wins, so any narrow allowance must follow the deny. MCP tools are a separate permission action
`<server>_<tool>` with resource `*` (for example `tvlock_tv_with`), so they stay callable.

Two caveats from OpenCode's own documentation, both to be verified with a live probe before
claiming enforcement:

1. Shell permission matching uses a best-effort command scanner; commands it cannot safely analyse
   (malformed syntax, dynamic evaluation such as `eval`) fail when a `shell` deny applies. Wrappers
   are not guaranteed to be recognised.
2. `deny` blocks the operation but the docs do not state that the tool schema is removed from the
   request. If hard enforcement beyond the scanner is needed, investigate `policies` with
   `action: "tool.use"` instead of assuming these rules are airtight.

## Lane identity and registration

Register under V2 `mcp.servers.tvlock` as a local stdio server, with `codemode: false` so its tools
are directly callable, and `pool` left at its default `false` so each checkout gets its own server
process and lane. The server binds to one checkout: every child command gets
`PLX_TV_LOCK_LANE=<that checkout>` in its environment, never a value from the model. A second
worktree is a different lane by construction. A per-project config file would be tracked by git, so
keep the registration in the global config.

## Lifecycle rules the server encodes

- Build first, acquire last — `deploy` already puts `tv-lock-require` last for this reason.
- One lease covers one bounded device run; release before any host-side work (reading captures,
  building, editing, writing Markdown).
- Release on error and on signal; TTL is the crash backstop, not a budget.
- `tv_acquire` is for a multi-command run only, and the lease is handed back at the end of that run.
- Auto-release at session end is **not** promised: it needs an OpenChamber disconnect/session-end
  notification to MCP servers, which has not been verified. Until then, TTL plus explicit release
  is the contract.

## Tests and acceptance

Host-only, no television:

- `tools/tv-lock.sh selftest` (contended acquire, live lease not stealable, expired lease
  reclaimable, owner-scoped release).
- `python3 .claude/hooks/tv-lock-guard-test.py` (the guard's classifier and lane rules; keep its
  false-positive cases green).
- New fake-TV tests for the MCP, with a stub lock tool behind an environment override: `IN USE:`
  refusal, release on a failing job, job allowlist, lane binding, and "raw `make deploy` is denied
  while `tvlock_tv_with` is allowed".

TV acceptance (owner, with the `tv-lock` skill): one real deploy/capture run driven entirely
through the MCP, with `tv-lock.sh status` confirming the lease was handed back. The owner chooses
the account state for that run; an agent must not assume a mock identity isolates it from real
watch-state writes. No server code is written before this design is agreed.

## Open questions before code

1. Does OpenChamber notify MCP servers when a session ends or disconnects? If not, do not promise
   auto-release; rely on explicit release and TTL.
2. Do the `shell` deny rules survive wrappers and Code Mode nested tools in a live probe? If not,
   decide whether `policies`/`tool.use` is warranted.
3. Which job allowlist is actually needed? Start with deploy, tests, capture and session drive.
4. Where should the server live? A small Python-stdlib stdio server under `tools/tvlock-mcp/` is
   the current preference: it only wraps existing shell CLIs, so no Rust crate is justified.

## Next step

If this design is approved: build the fake-TV harness and server, register it locally, run the
permission probe, then ask the owner for one real TV acceptance run. Nothing here commits or
pushes anything.

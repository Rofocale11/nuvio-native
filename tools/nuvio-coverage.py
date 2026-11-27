#!/usr/bin/env python3
"""Write docs/nuvio/UPSTREAM-COVERAGE.md from the generated scene set and a host run log.

The report is the answer to "does the fork cover upstream's test scenarios": which scenarios
are translated, which are skipped and why, which passed a host smoke run, and which gates were
deferred because they are calibrated to the television or to a full multi-minute bench window.
A host PASS is a logic/budget signal only; it is never a device verdict.
"""

from __future__ import annotations

import argparse
import json
import re
import sys
from collections import Counter
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
SCENES = REPO / "tests" / "nuvio-upstream-scenes.json"
FORK_SCENES = REPO / "tests" / "nuvio-fork-scenes.json"
DEFAULT_OUT = REPO / "docs" / "nuvio" / "UPSTREAM-COVERAGE.md"


def parse_results(path: Path | None) -> dict[str, tuple[str, str]]:
    if path is None or not path.is_file():
        return {}
    out: dict[str, tuple[str, str]] = {}
    for line in path.read_text(encoding="utf-8", errors="replace").splitlines():
        match = re.match(r"^(PASS|FAIL) (\S+) — (.*)$", line)
        if match:
            out[match.group(2)] = (match.group(1), match.group(3))
    return out


def build(data: dict, results: dict[str, tuple[str, str]], fork: dict | None = None) -> str:
    scenes = data.get("scenes", [])
    skipped = data.get("skipped", [])
    names = [s["name"] for s in scenes]
    verdicts = Counter(results.get(name, ("NOT RUN", ""))[0] for name in names)
    gates = Counter(
        gate.strip()
        for name in names
        for gate in results.get(name, ("", ""))[1].split("HOST-DEFERRED:")[-1].split(";")
        if ":" in gate
    )
    lines = [
        "# Upstream scenario coverage in Nuvio Native",
        "",
        "Generated from `tests/manifest.json` by `tools/nuvio-scenes.py`. A host PASS from",
        "`tools/nuvio-sim-scenes.py` is **not** a TV verdict: simulator frame rates are uncapped and",
        "the gates calibrated to the LG set or to a full bench window are listed as deferred.",
        "",
        "## Summary",
        "",
        f"- Upstream UI fps scenes translated: **{len(scenes)}**",
        f"- Upstream UI fps scenes skipped: **{len(skipped)}** (see reasons below)",
        f"- Host smoke results: **{verdicts['PASS']} PASS / {verdicts['FAIL']} FAIL / "
        f"{verdicts['NOT RUN']} not run**",
        "- Player tier, playback cases and pipeline cases are **not covered** by this harness "
        "(Stage 4: they need PMS/stream fixtures the in-process shim does not serve).",
    ]
    fork_scenes = (fork or {}).get("scenes", [])
    if fork_scenes:
        fork_verdicts = Counter(results.get(s["name"], ("NOT RUN", ""))[0] for s in fork_scenes)
        lines += [
            f"- Fork behavior scenes — all 16 `docs/nuvio/SCENARIO.md` pins plus the source "
            f"picker, episode click, subtitle drill and catalog-shape checks: "
            f"**{len(fork_scenes)}** — "
            f"{fork_verdicts['PASS']} PASS / {fork_verdicts['FAIL']} FAIL / "
            f"**{fork_verdicts['NOT RUN']} not run**",
            "- Routine coverage uses the deterministic local Stremio addon "
            "(`tools/nuvio-fixture-addon.py`) and a loopback TMDB API/image fixture "
            "(`tools/nuvio-tmdb-fixture.py`, via `TMDB_API_BASE` / `TMDB_IMAGE_BASE`); `nuviolive` "
            "with public addons stays the one-off realism check, so no routine scene depends on a "
            "rate-limited CDN or on TMDB.",
            "- Run the whole fork set fast with "
            "`python3 tools/nuvio-sim-scenes.py --scenes tests/nuvio-fork-scenes.json --quick` "
            "(3 parallel boots, shortened dwells; fps scenes stay serial at full length).",
            "- S8 (About copy), S11 (P2P switch state) and S14 (clear logo / hero plot) produce "
            "PNGs: their pass signal is partly visual and must be read from the shot, not from "
            "the event log alone.",
        ]
    if gates:
        lines += ["", "Deferred host gates:", ""]
        for gate, count in gates.most_common():
            lines.append(f"- `{gate.split(':')[0].strip()}` — {count} scene(s)")
    lines += ["", "## Skipped upstream scenes", "", "| Scene | Reason |", "|---|---|"]
    for item in skipped:
        lines.append(f"| {item.get('name')} | {item.get('reason', '')} |")
    lines += ["", "## Translated scenes", "", "| Nuvio scene | Upstream source | Host verdict | Notes |",
              "|---|---|---|---|"]
    for scene in scenes:
        verdict, detail = results.get(scene["name"], ("NOT RUN", ""))
        note = detail.split(" | HOST-DEFERRED:")[0]
        if len(note) > 160:
            note = note[:157] + "…"
        lines.append(f"| `{scene['name']}` | `{scene.get('source', '')}` | {verdict} | {note} |")
    if fork_scenes:
        lines += ["", "## Fork-specific scenes", "", "| Scene | What it pins | Host verdict | Notes |",
                  "|---|---|---|---|"]
        for scene in fork_scenes:
            verdict, detail = results.get(scene["name"], ("NOT RUN", ""))
            note = detail.split(" | HOST-DEFERRED:")[0]
            if len(note) > 160:
                note = note[:157] + "…"
            lines.append(f"| `{scene['name']}` | {scene.get('note', '')} | {verdict} | {note} |")
    lines += [
        "",
        "## What 'covered' means here",
        "",
        "A translated scene boots the real app in the sealed nuviolive profile with upstream's",
        "own dev triggers, route/overlay and key walk, and is graded by `tests/run.py`'s",
        "heartbeat parsers. Reach assertions and page-landing checks from upstream's `walk`",
        "blocks are not yet implemented in the fork, and the generated key sequences are",
        "approximations sized from the manifest rather than from the live catalog.",
        "",
        "Regenerate with `tools/nuvio-items.py`, `tools/nuvio-scenes.py` and",
        "`tools/nuvio-sim-scenes.py --max-run-s N`; pass the run log to this tool with `--results`.",
        "",
    ]
    return "\n".join(lines)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("--scenes", type=Path, default=SCENES)
    parser.add_argument("--fork-scenes", type=Path, default=FORK_SCENES)
    parser.add_argument("--results", type=Path, help="host run log from nuvio-sim-scenes.py")
    parser.add_argument("--out", type=Path, default=DEFAULT_OUT)
    args = parser.parse_args(argv)
    try:
        data = json.loads(args.scenes.read_text(encoding="utf-8"))
        fork = json.loads(args.fork_scenes.read_text(encoding="utf-8")) if args.fork_scenes.is_file() else None
    except (OSError, json.JSONDecodeError) as exc:
        print(f"nuvio-coverage: {exc}", file=sys.stderr)
        return 1
    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_text(build(data, parse_results(args.results), fork), encoding="utf-8")
    print(f"nuvio-coverage: wrote {args.out}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

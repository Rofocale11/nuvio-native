#!/usr/bin/env python3
"""Translate upstream's UI fps scenes into the fork's Nuvio TV scene file.

Upstream's ``tests/manifest.json`` is the authority for routes, overlays, dev triggers and
grading gates. This fork cannot run the player/playback tiers through the in-process shim, and
upstream itself skips an ``item`` key its installation cannot resolve, so this generator applies
the same rule: a scene needing an unmapped symbolic item is recorded in `skipped`, not emitted.

Trigger names lose their ``plxnative-`` prefix: ``nuvio-tv.py`` passes them to
``tv-session.sh up --arm``, which adds that prefix itself. `$rk` is substituted from
``tests/nuvio-items.json``, which ``tools/nuvio-items.py`` derives from a sealed nuviolive boot.
The generated file is separate from the hand-written ``tests/nuvio-tv-scenes.json`` and is
selected with ``nuvio-tv.py --scenes``.
"""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
MANIFEST = REPO / "tests" / "manifest.json"
ITEMS = REPO / "tests" / "nuvio-items.json"
DEFAULT_OUT = REPO / "tests" / "nuvio-upstream-scenes.json"

GATE_FIELDS = (
    "loop_floor", "fps_floor", "fps_ceiling", "worst_ceiling_ms", "stall_ceiling_ms",
    "coldopen_ceiling_ms", "bench_missed_max", "bench_drift_ms", "bench_rss_growth_kb",
    "bench_depth_rss_kb", "bench_root_rss_kb", "upload_per_frame_min",
    "upload_late_share_max", "upload_frames_min", "fps_floor_windows",
)
FRAME_GATES = ("worst_ceiling_ms", "stall_ceiling_ms", "bench_missed_max")

SCREEN_BY_OVERLAY = {"account": "account", "itemmenu": "itemmenu", "alert": "home"}
SCREEN_BY_ROUTE = {
    "home": "home", "library": "library", "detail": "home", "collection": "home",
    "person": "home", "search": "home", "watchlist": "home", "catalog": "home",
    "login": "login", "onboard": "home", "player": "home",
}


def _walk_keys(walk: dict) -> tuple[list[str], float, float] | None:
    """Approximate upstream's KeyWalk block as nuvio-tv's keys/gap/pause protocol.

    Upstream sizes walks from the live catalog and checks reach/page-landing log lines. Those
    checks are not expressible in the fork's scene file yet; what IS expressible is the key
    sequence itself. A scene whose walk has no recognisable shape is skipped, never emitted as
    a no-op.
    """
    gap = float(walk.get("key_gap_s", walk.get("key_ms", 330) / 1000.0))
    rest = float(walk.get("rest_s", walk.get("rest_ms", 2000) / 1000.0))
    if "burst" in walk:
        count = int(walk["burst"]) * int(walk.get("bursts", 1))
        return (["down"] * count + ["up"] * count, gap, rest)
    cards = int(walk.get("cards", 0))
    rows = int(walk.get("row_down", 0) or walk.get("max_rows", 0) or walk.get("landed", 0))
    if not cards and not rows:
        return None
    forward = ["down"] * rows + ["right"] * cards
    backward = ["left"] * cards + ["up"] * rows
    if walk.get("back"):
        backward.append("back")
    return (forward + backward, gap, rest)


def translate(manifest: dict, items: dict, *, prefix: str = "up-") -> dict:
    mapping = items.get("items", {})
    scenes: list[dict] = []
    skipped: list[dict] = []
    for source in manifest.get("fps_scenes", []):
        if source.get("tier") != "ui":
            skipped.append({"name": source.get("name"), "reason": "player tier (Stage 4)"})
            continue
        name = source["name"]
        item = source.get("item")
        rk = mapping.get(item) if item else None
        if item and rk is None:
            skipped.append({"name": name, "reason": f"unmapped item key {item!r}"})
            continue

        triggers: list[str] = []
        for raw_name, value in source.get("triggers", {}).items():
            # nuvio-tv.py arms via tv-session --arm, which prepends plxnative-.
            short = raw_name.removeprefix("plxnative-")
            if short == "framedrop":
                continue  # the scene's `framedrop` field arms the detector with a threshold
            if value is True or value is None:
                text = short
            else:
                text = f"{short}={str(value).replace('$rk', rk or '')}"
            triggers.append(text)

        walk = source.get("walk")
        keys: list[str] = []
        key_gap = 0.5
        pause_s = 2.0
        walk_note = ""
        if walk:
            translated = _walk_keys(walk)
            if translated is None:
                skipped.append({"name": name, "reason": "untranslatable KeyWalk block"})
                continue
            keys, key_gap, pause_s = translated
            walk_note = " Key walk approximated from upstream's `walk` block; reach and landing assertions are not yet graded in the fork."
        else:
            walk_note = " Oscillator-driven; no explicit key walk upstream."

        overlay = source.get("overlay")
        route = source["route"]
        screen = SCREEN_BY_OVERLAY.get(overlay, SCREEN_BY_ROUTE.get(route, "home"))
        scene: dict = {
            "name": f"{prefix}{name}",
            "screen": screen,
            "route": route,
            "settle_s": float(source.get("warmup_s", 6)),
            "run_s": float(source.get("run_secs", 18)),
            "warmup_s": float(source.get("warmup_s", 5)),
            "keys": keys,
            "expect": [],
            "triggers": triggers,
            "source": name,
            "note": f"Generated from tests/manifest.json::{name}. "
                    f"{source.get('comment', '')}{walk_note}".strip(),
        }
        if keys:
            scene["key_gap"] = key_gap
            scene["pause_after"] = len(keys) // 2
            scene["pause_s"] = pause_s
        if overlay:
            scene["overlay"] = overlay
        if item:
            scene["item"] = item
            scene["ratingKey"] = rk
        for field in GATE_FIELDS:
            if field in source:
                scene[field] = source[field]
        if any(field in scene for field in FRAME_GATES):
            scene["framedrop"] = 22
        scenes.append(scene)

    return {
        "_comment": (
            "Generated by tools/nuvio-scenes.py from tests/manifest.json. UI tier only; "
            "$rk resolved from the sealed-profile tests/nuvio-items.json. Select with "
            "nuvio-tv.py --scenes tests/nuvio-upstream-scenes.json. Thresholds are upstream's; "
            "calibrate each on the debug TV before treating a PASS as device evidence."
        ),
        "source": "tests/manifest.json (fps_scenes, tier=ui)",
        "items_source": "tests/nuvio-items.json",
        "scenes": scenes,
        "skipped": skipped,
    }


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("--manifest", type=Path, default=MANIFEST)
    parser.add_argument("--items", type=Path, default=ITEMS)
    parser.add_argument("--out", type=Path, default=DEFAULT_OUT)
    parser.add_argument("--prefix", default="up-")
    args = parser.parse_args(argv)
    try:
        manifest = json.loads(args.manifest.read_text(encoding="utf-8"))
        items = json.loads(args.items.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as exc:
        print(f"nuvio-scenes: {exc}", file=sys.stderr)
        return 1
    result = translate(manifest, items, prefix=args.prefix)
    args.out.write_text(json.dumps(result, indent=2) + "\n", encoding="utf-8")
    print(f"nuvio-scenes: wrote {args.out} — {len(result['scenes'])} scenes, "
          f"{len(result['skipped'])} skipped")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

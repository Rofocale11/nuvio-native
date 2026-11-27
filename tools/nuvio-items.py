#!/usr/bin/env python3
"""Derive the fork's symbolic-item -> ratingKey map from a settled nuviolive boot.

Upstream's ``tests/run.py`` resolves an ``item`` key from a gitignored, owner-private
``manifest.local.json`` inventory against a real PMS. This fork has no Plex library and must
never read that file: its items are the synthetic keys the in-process shim assigns to the
benchmark addon's catalog cards. We boot the real app once with the sealed nuviolive identity,
read the two tab pages by accessibility id (``nav.movies`` / ``nav.shows``), and write only the
numeric ratingKeys to ``tests/nuvio-items.json``. No titles, tokens or account data are stored.

The output is a coverage input, not a claim that every upstream shape exists here. Item keys
that need a specific codec, subtitle or resume shape stay unmapped and are reported as skipped
by ``tools/nuvio-scenes.py``, the same way upstream skips a key its library cannot exercise.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import signal
import subprocess
import sys
import tempfile
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
SIM = REPO / "tools" / "nuvio-sim.py"
A11Y = REPO / "tools" / "a11y.py"
BENCH_ADDONS = REPO / "tests" / "nuvio-tv-benchmark-addons.json"
DEFAULT_OUT = REPO / "tests" / "nuvio-items.json"

# Keys that describe a title SHAPE rather than a codec/resume fixture. A movie or a show is
# enough for the UI tiers; the codec-specific keys belong to the player tier (Stage 4).
MOVIE_KEYS = (
    "movie_in_home_catalog",
    "movie_with_related",
    "movie_cast0_in_both_libraries",
    "collection_member_movie",
    "collection",
    "movie_h264_ac3_1080p",
)
SHOW_KEYS = (
    "show_detail",
    "show_long_season",
)


def _call(cmd: list[str], *, timeout: int = 300) -> subprocess.CompletedProcess[str]:
    return subprocess.run(cmd, capture_output=True, text=True, timeout=timeout)


def _boot(addons: Path, settle: float, section: int) -> tuple[Path, int]:
    """Boot into a shim library section: 1 is Movies, 2 is TV Shows (shim_pms.rs)."""
    boot = _call([
        sys.executable, str(SIM), "run",
        "--addons", str(addons), "--settle", str(settle),
        "--trigger", f"library={section}", "--keep",
    ], timeout=600)
    if boot.returncode:
        raise RuntimeError(f"nuvio-sim boot failed:\n{boot.stdout}\n{boot.stderr}")
    runtime_match = re.search(r"boot plxnative-sim in (\S+)", boot.stdout)
    pid_match = re.search(r"left app pid (\d+) running", boot.stdout)
    if not runtime_match or not pid_match:
        raise RuntimeError(f"could not parse simulator boot output:\n{boot.stdout}")
    return Path(runtime_match.group(1)), int(pid_match.group(1))


def _subjects(runtime: Path) -> list[str]:
    state = _call([sys.executable, str(A11Y), "state", str(runtime)])
    if state.returncode:
        raise RuntimeError(f"a11y state: {state.stderr.strip()}")
    snap = json.loads(state.stdout)
    out: list[str] = []
    for element in snap.get("elements", []):
        subject = element.get("subject")
        if subject is None:
            continue
        text = str(subject)
        if text.isdigit() and text not in out:
            out.append(text)
    return out


def build(addons: Path, settle: float) -> dict:
    runtime, pid = _boot(addons, settle, 1)
    try:
        movies = _subjects(runtime)
    finally:
        try:
            os.kill(pid, signal.SIGTERM)
        except ProcessLookupError:
            pass
    runtime, pid = _boot(addons, settle, 2)
    try:
        shows = _subjects(runtime)
    finally:
        try:
            os.kill(pid, signal.SIGTERM)
        except ProcessLookupError:
            pass
    if not movies or not shows:
        raise RuntimeError(
            f"no card subjects found (movies={len(movies)}, shows={len(shows)}); "
            "the catalogs did not land"
        )
    items: dict[str, str] = {}
    for index, key in enumerate(MOVIE_KEYS):
        items[key] = movies[index % len(movies)]
    for index, key in enumerate(SHOW_KEYS):
        items[key] = shows[index % len(shows)]
    return {
        "_comment": (
            "Fork-owned: synthetic ratingKeys from a sealed nuviolive boot against the benchmark "
            "addon. No owner account, token, title or private manifest.local.json is read. "
            "Regenerate with tools/nuvio-items.py when the addon list changes."
        ),
        "source": "nuviolive + tests/nuvio-tv-benchmark-addons.json",
        "movies_seen": len(movies),
        "shows_seen": len(shows),
        "items": items,
        "unmapped": [
            "episode_hevc_4k_hdr10_eac3", "movie_av1_no_dp_audio",
            "movie_hevc_4k_dovi_p8", "episode_h264_aac",
            "movie_h264_ac3_many_audio", "movie_hevc_4k_hdr10_truehd",
            "movie_hevc_aac_mp4", "movie_hevc_4k_pgs_subs",
        ],
    }


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("--addons", type=Path, default=BENCH_ADDONS)
    parser.add_argument("--out", type=Path, default=DEFAULT_OUT)
    parser.add_argument("--settle", type=float, default=8.0)
    args = parser.parse_args(argv)
    try:
        data = build(args.addons, args.settle)
    except (RuntimeError, subprocess.TimeoutExpired, json.JSONDecodeError) as exc:
        print(f"nuvio-items: {exc}", file=sys.stderr)
        return 1
    args.out.write_text(json.dumps(data, indent=2) + "\n", encoding="utf-8")
    print(f"nuvio-items: wrote {args.out} ({len(data['items'])} mapped, "
          f"{len(data['unmapped'])} player-tier shapes unmapped)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

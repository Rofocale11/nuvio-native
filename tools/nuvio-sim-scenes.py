#!/usr/bin/env python3
"""Host-first coverage runner: drive generated scenes in the simulator.

Two scene families are supported:

* upstream fps scenes (``tests/nuvio-upstream-scenes.json``), graded with the upstream
  harness's own heartbeat gates; and
* fork behavior scenes (``tests/nuvio-fork-scenes.json``), graded by the app's own a11y
  snapshot and event log — source picker, episode click, subtitle drill-down, and the
  other Nuvio-only paths upstream's manifest never described.

Both families can ask for a deterministic local Stremio addon via ``mock_addon_args``; the
runner starts ``tools/nuvio-fixture-addon.py`` for the duration of the boot and points the
sealed nuviolive profile at it. No television, lock or owner action is needed; the simulator
cannot grade LG rasterization, the video plane or AV, so a PASS here is a logic/budget signal
only.
"""

from __future__ import annotations

import argparse
import importlib.util
import json
import os
import re
import socket
import subprocess
import sys
import tempfile
import time
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
SIM = REPO / "tools" / "nuvio-sim.py"
TV = REPO / "tools" / "nuvio-tv.py"
MOCK_ADDON = REPO / "tools" / "nuvio-fixture-addon.py"
TMDB_FIXTURE = REPO / "tools" / "nuvio-tmdb-fixture.py"
BENCH_ADDONS = REPO / "tests" / "nuvio-tv-benchmark-addons.json"
DEFAULT_SCENES = REPO / "tests" / "nuvio-upstream-scenes.json"
MEDIA_PLACEHOLDER = "@FIXTURE_MEDIA@"
# S5/S13 of docs/nuvio/SCENARIO.md use this H.264 sample: the smaller stand-in is MPEG-4 Part 2,
# which the host framing path refuses. Override with NUVIOTV_FIXTURE_MEDIA if it moves.
DEFAULT_MEDIA = Path.home() / ".cache/plxnative-demo/src/sintel-film.mp4"


def _load_tv():
    spec = importlib.util.spec_from_file_location("nuvio_tv_for_sim", TV)
    if spec is None or spec.loader is None:
        raise RuntimeError("could not load tools/nuvio-tv.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


# Upstream gates calibrated on the Mali set or on a full multi-minute bench window cannot be
# evaluated in a capped simulator smoke run. Deferring them here is explicit and reported; it does
# NOT touch tools/nuvio-tv.py, so device grading keeps every gate.
HOST_DEFER = {
    "upload_per_frame_min": "needs the device's poster-upload heartbeat on a warm catalog",
    "upload_late_share_max": "needs the device's poster-upload heartbeat on a warm catalog",
    "upload_frames_min": "the shim caches art; an already-warm library uploads no posters",
    "coldopen_ceiling_ms": "cold-mount timing is device-only",
    "bench_missed_max": "needs the full bench window on the device",
    "bench_drift_ms": "needs the full bench window on the device",
    "bench_rss_growth_kb": "device RSS accounting",
    "bench_depth_rss_kb": "device RSS accounting",
    "bench_root_rss_kb": "device RSS accounting",
}


def free_port() -> int:
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as sock:
        sock.bind(("127.0.0.1", 0))
        return int(sock.getsockname()[1])


def fixture_media() -> Path | None:
    """The small local MP4 the fork's playback fixtures serve; never a tracked repo binary."""
    candidates = [os.environ.get("NUVIOTV_FIXTURE_MEDIA"), str(DEFAULT_MEDIA)]
    for value in candidates:
        if value and Path(value).is_file():
            return Path(value).resolve()
    return None


def start_mock_addon(scene: dict, *, timeout: float):
    """Boot the local Stremio mock for one scene; return (proc, addon_json, cleanup)."""
    spec = [str(a) for a in scene.get("mock_addon_args", [])]
    if not spec:
        return None, None, None
    media = fixture_media()
    resolved = []
    for arg in spec:
        if arg == MEDIA_PLACEHOLDER:
            if media is None:
                raise RuntimeError("no local fixture MP4 (set NUVIOTV_FIXTURE_MEDIA)")
            resolved.append(str(media))
        else:
            resolved.append(arg)
    port = free_port()
    proc = subprocess.Popen(
        [sys.executable, str(MOCK_ADDON), "--port", str(port), *resolved],
        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        try:
            with socket.create_connection(("127.0.0.1", port), timeout=0.5):
                break
        except OSError:
            if proc.poll() is not None:
                raise RuntimeError("the local mock addon exited during boot")
            time.sleep(0.1)
    else:
        proc.terminate()
        raise RuntimeError("the local mock addon never listened")
    addon_file = tempfile.NamedTemporaryFile(mode="w", suffix=".json", prefix="nuvio-mock-addons-",
                                             delete=False)
    urls = [f"http://127.0.0.1:{port}"] + [str(u) for u in scene.get("mock_addon_extra_urls", [])]
    json.dump({"addons": urls}, addon_file)
    addon_file.close()

    def cleanup():
        if proc.poll() is None:
            proc.terminate()
            try:
                proc.wait(timeout=5)
            except subprocess.TimeoutExpired:
                proc.kill()
        Path(addon_file.name).unlink(missing_ok=True)

    return proc, Path(addon_file.name), cleanup


def start_tmdb_fixture(scene: dict, *, timeout: float):
    """Serve TMDB API + image bytes locally; return (proc, env_overrides, cleanup)."""
    port = free_port()
    proc = subprocess.Popen(
        [sys.executable, str(TMDB_FIXTURE), "--port", str(port)],
        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        try:
            with socket.create_connection(("127.0.0.1", port), timeout=0.5):
                break
        except OSError:
            if proc.poll() is not None:
                raise RuntimeError("the local TMDB fixture exited during boot")
            time.sleep(0.1)
    else:
        proc.terminate()
        raise RuntimeError("the local TMDB fixture never listened")

    def cleanup():
        if proc.poll() is None:
            proc.terminate()
            try:
                proc.wait(timeout=5)
            except subprocess.TimeoutExpired:
                proc.kill()

    return proc, {
        "TMDB_API_BASE": f"http://127.0.0.1:{port}/3",
        "TMDB_IMAGE_BASE": f"http://127.0.0.1:{port}/t/p",
    }, cleanup


def grade_behavior(scene: dict, lines: list[str], snapshot: dict | None) -> tuple[bool, str]:
    """Grade a fork behavior scene from the event log and the live a11y snapshot."""
    text = "\n".join(lines)
    details: list[str] = []
    ok = True
    for want in scene.get("expect_lines", []):
        hit = want in text
        ok = ok and hit
        details.append(f"{'FOUND' if hit else 'MISSING'} {want!r}")
    for pattern in scene.get("expect_regex", []):
        found = re.search(pattern, text, re.MULTILINE)
        ok = ok and bool(found)
        details.append(f"{'MATCH' if found else 'NO MATCH'} /{pattern}/")
    for unwanted in scene.get("expect_absent", []):
        hit = unwanted in text
        ok = ok and not hit
        details.append(f"{'PRESENT' if hit else 'ABSENT'} {unwanted!r}")

    checks = scene.get("a11y_checks", [])
    if checks:
        if not isinstance(snapshot, dict):
            details.append("no a11y snapshot (the scene asked for one but got none)")
            return False, " | ".join(details)
        elements = snapshot.get("elements", [])
        ids = [e.get("id") for e in elements if isinstance(e, dict)]
        route = snapshot.get("route")
        focused = snapshot.get("focused")
        for check in checks:
            if "route" in check:
                hit = route == check["route"]
                ok = ok and hit
                details.append(f"route {route!r} == {check['route']!r}: {'PASS' if hit else 'FAIL'}")
            if "focused" in check:
                hit = focused == check["focused"]
                ok = ok and hit
                details.append(f"focused {focused!r} == {check['focused']!r}: {'PASS' if hit else 'FAIL'}")
            for want in check.get("has_ids", []):
                hit = want in ids
                ok = ok and hit
                details.append(f"a11y id {want}: {'FOUND' if hit else 'MISSING'}")
            for unwanted in check.get("absent_ids", []):
                hit = unwanted in ids
                ok = ok and not hit
                details.append(f"a11y id {unwanted}: {'PRESENT' if hit else 'ABSENT'}")
            for count_check in check.get("min_count", []):
                prefix = count_check["prefix"]
                n = int(count_check["n"])
                found = sum(1 for i in ids if isinstance(i, str) and i.startswith(prefix))
                hit = found >= n
                ok = ok and hit
                details.append(f"{prefix} count {found} >= {n}: {'PASS' if hit else 'FAIL'}")
            for count_check in check.get("max_count", []):
                prefix = count_check["prefix"]
                n = int(count_check["n"])
                found = sum(1 for i in ids if isinstance(i, str) and i.startswith(prefix))
                hit = found <= n
                ok = ok and hit
                details.append(f"{prefix} count {found} <= {n}: {'PASS' if hit else 'FAIL'}")
    if not details:
        return False, "a behavior scene declared no assertions"
    return bool(ok), " | ".join(details)


# Upstream-derived scenes carry no fixture declaration: give them the same deterministic
# catalog/TMDB shape as the fork scenes unless the caller explicitly opts out with --no-local.
DEFAULT_MOCK_ARGS = ["--movies", "300", "--shows", "12", "--home-hubs", "41",
                     "--section-hubs", "2", "--cw-art-fixture", "--sources", "8"]


def run_scene(tv, scene: dict, scenes_path: Path, addons: Path | None, *,
              timeout: int, max_run_s: float, pms_port: int = 32400,
              fast: float = 1.0, local: bool = True) -> tuple[bool, str, str | None]:
    if scene.get("fake_mode"):
        local = False
    if local:
        if not scene.get("mock_addon_args"):
            scene = dict(scene)
            scene["mock_addon_args"] = list(DEFAULT_MOCK_ARGS)
        if not scene.get("local_tmdb"):
            scene = dict(scene)
            scene["local_tmdb"] = True
    tv.SCENES_OVERRIDE = scenes_path.resolve()
    harness = None
    if not scene.get("behavior"):
        harness = tv.fps_module()
        # Upstream refuses `sim=1` logs because its ceilings are calibrated to the Mali set.
        # This runner EXISTS to grade simulator logs, so opt in explicitly and label every
        # verdict as a host-only signal. tools/nuvio-tv.py does not patch this: TV gating stays
        # strict.
        harness.reject_simulator = lambda lines: None
    cmd = [
        sys.executable, str(SIM), "run",
        "--addons", str(addons or BENCH_ADDONS),
        "--port", str(pms_port),
        "--settle", str(max(2.0, float(scene.get("settle_s", 8)) * fast)),
        "--run-s", str(min(max(float(scene.get("run_s", 18)) * fast,
                               1.0 if scene.get("behavior") else 6.0), max_run_s)),
        "--key-gap", str(max(0.3, float(scene.get("key_gap", 0.5)) * fast)),
    ]
    keys = scene.get("keys", [])
    if keys:
        cmd += ["--keys", ",".join(keys)]
    for trigger in scene.get("triggers", []):
        cmd += ["--trigger", trigger]
    if scene.get("framedrop") is not None:
        cmd += ["--trigger", f"framedrop={scene['framedrop']}"]
    if scene.get("fake_mode"):
        cmd += ["--fake"]
    if scene.get("shot"):
        cmd += ["--shot", str(scene["shot"])]
    if scene.get("host_engine"):
        cmd += ["--host-engine"]
    if scene.get("log_reset"):
        cmd += ["--no-final-proof"]

    mock_proc, mock_addons, mock_cleanup = (None, None, None)
    tmdb_proc, tmdb_env, tmdb_cleanup = (None, {}, None)
    snapshot_path = None
    try:
        if scene.get("local_tmdb"):
            try:
                tmdb_proc, tmdb_env, tmdb_cleanup = start_tmdb_fixture(scene, timeout=timeout)
            except RuntimeError as exc:
                return False, f"local TMDB fixture: {exc}", None
        try:
            mock_proc, mock_addons, mock_cleanup = start_mock_addon(scene, timeout=timeout)
        except RuntimeError as exc:
            return False, f"local mock addon: {exc}", None
        if mock_addons is not None:
            cmd[cmd.index("--addons") + 1] = str(mock_addons)
        if scene.get("a11y_checks"):
            handle = tempfile.NamedTemporaryFile(suffix=".json", prefix="nuvio-a11y-", delete=False)
            snapshot_path = Path(handle.name)
            handle.close()
            snapshot_path.unlink(missing_ok=True)
            cmd += ["--a11y-state", str(snapshot_path)]
        child_env = os.environ.copy()
        child_env.update(tmdb_env)
        result = subprocess.run(cmd, capture_output=True, text=True, timeout=timeout, env=child_env)
        log_match = re.search(r"\(log: (\S+)\)", result.stdout + result.stderr)
        if result.returncode or not log_match:
            tail = (result.stdout + result.stderr).strip().splitlines()[-4:]
            return False, "simulator run failed: " + " | ".join(tail), None
        log_path = log_match.group(1)
        lines = Path(log_path).read_text(encoding="utf-8", errors="replace").splitlines()
        if scene.get("behavior"):
            snapshot = None
            if snapshot_path is not None and snapshot_path.is_file():
                try:
                    snapshot = json.loads(snapshot_path.read_text(encoding="utf-8"))
                except (OSError, json.JSONDecodeError):
                    snapshot = None
            ok, detail = grade_behavior(scene, lines, snapshot)
        else:
            gradeable = dict(scene)
            deferred = [f"{gate}: {why}" for gate, why in HOST_DEFER.items() if gate in gradeable]
            for gate in HOST_DEFER:
                gradeable.pop(gate, None)
            ok, detail = tv.grade_scene(gradeable, lines, harness)
            if deferred:
                detail += " | HOST-DEFERRED: " + "; ".join(deferred)
        return bool(ok), detail, log_path
    finally:
        if snapshot_path is not None:
            snapshot_path.unlink(missing_ok=True)
        if mock_cleanup is not None:
            mock_cleanup()
        if tmdb_cleanup is not None:
            tmdb_cleanup()


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("--scene", help="run one scene by name")
    parser.add_argument("--scenes", type=Path, default=DEFAULT_SCENES)
    parser.add_argument("--addons", type=Path, help="addon JSON (default: benchmark catalog)")
    parser.add_argument("--timeout", type=int, default=900)
    parser.add_argument("--max-run-s", type=float, default=25.0,
                        help="cap each scene's run window for a host smoke pass (default 25 s)")
    parser.add_argument("--local", dest="local", action="store_true", default=True,
                        help="use the loopback addon + TMDB fixtures for every scene (default)")
    parser.add_argument("--no-local", dest="local", action="store_false",
                        help="run against the public benchmark addons and real TMDB")
    parser.add_argument("--quick", action="store_true",
                        help="stable fast profile: --jobs 3 --fast 0.5 (about 4-5x, all pins pass)")
    parser.add_argument("--jobs", type=int, default=None,
                        help="parallel sealed boots (default 1; >1 makes host fps numbers noisy)")
    parser.add_argument("--fast", type=float, default=None,
                        help="multiply settle/key-gap/run windows (e.g. 0.5 for a quick pass)")
    args = parser.parse_args(argv)
    if args.quick:
        args.jobs = args.jobs if args.jobs is not None else 3
        args.fast = args.fast if args.fast is not None else 0.5
    args.jobs = 1 if args.jobs is None else args.jobs
    args.fast = 1.0 if args.fast is None else args.fast
    try:
        data = json.loads(args.scenes.read_text(encoding="utf-8"))
        tv = _load_tv()
    except (OSError, json.JSONDecodeError, RuntimeError) as exc:
        print(f"nuvio-sim-scenes: {exc}", file=sys.stderr)
        return 2
    scenes = data.get("scenes", [])
    if args.scene:
        scenes = [s for s in scenes if s.get("name") == args.scene]
        if not scenes:
            print(f"nuvio-sim-scenes: no scene named {args.scene!r}", file=sys.stderr)
            return 2
    # Each parallel boot needs its own PMS port: the shim binds the argv port, so two sims on
    # 32400 would fight over the socket. Addon and TMDB fixtures already take free loopback ports.
    used_ports: set[int] = set()
    ports = []
    for _ in scenes:
        while True:
            port = free_port()
            if port not in used_ports:
                used_ports.add(port)
                ports.append(port)
                break

    def one(scene_and_port):
        scene, port = scene_and_port
        return run_scene(tv, scene, args.scenes, args.addons,
                         timeout=args.timeout, max_run_s=args.max_run_s,
                         pms_port=port, fast=args.fast, local=args.local)

    # An fps scene measures the host compositor: running three boots at once starves it and the
    # loop/fps floors are no longer meaningful. Those scenes opt into a serial, full-length pass;
    # behavior scenes (a11y/log assertions) are safe to parallelize and shorten.
    serial = [(scene, port) for scene, port in zip(scenes, ports) if scene.get("serial")]
    parallel = [(scene, port) for scene, port in zip(scenes, ports) if not scene.get("serial")]
    results: dict[str, tuple[bool, str, str | None]] = {}
    for scene, port in serial:
        ok, detail, log = run_scene(tv, scene, args.scenes, args.addons,
                                    timeout=args.timeout, max_run_s=args.max_run_s,
                                    pms_port=port, fast=1.0, local=args.local)
        results[scene["name"]] = (ok, detail, log)
    if args.jobs > 1:
        with ThreadPoolExecutor(max_workers=args.jobs) as pool:
            for scene, result in zip((s for s, _ in parallel),
                                     pool.map(one, parallel)):
                results[scene["name"]] = result
    else:
        for scene, port in parallel:
            results[scene["name"]] = one((scene, port))
    passed = 0
    for scene in scenes:
        ok, detail, log = results[scene["name"]]
        passed += bool(ok)
        print(f"{'PASS' if ok else 'FAIL'} {scene['name']} — {detail}"
              + (f"\n     log: {log}" if log else ""), flush=True)
    print(f"nuvio-sim-scenes: {passed}/{len(scenes)} PASS (host only, no TV verdict)")
    return 0 if passed == len(scenes) else 1


if __name__ == "__main__":
    raise SystemExit(main())

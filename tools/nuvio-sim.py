#!/usr/bin/env python3
"""Drive the LIVE Nuvio simulator profile in one command — no TV, no account, no owner.

This is the agent host loop. `plxnative-nuviolive` boots the *real* app (same in-process shim,
same data layer, real public Stremio addons and TMDB) with a sealed dev identity and a private
redb. Agents change code, run this, look at the shot and the event log, and repeat.

    make sim-nuvio                 # once, on macOS (Linux/WSLg: make sim-nuvio-linux)
    python3 tools/nuvio-sim.py selftest
    python3 tools/nuvio-sim.py --keys "down,down,right,ok" --shot home

TMDB_API_KEY is the dedicated, non-household key already in the gitignored
`rust-modules/stremio/nuvio.local.env` (the production key is a different, GitHub-secret value).
An exported environment variable wins; otherwise this script reads that one file. It never prints
the value, never writes it into the instance root, and never bakes it into a binary.

The public addon list is a tracked, account-free JSON file copied into the instance root; override
it with `--addons PATH`. The household's addons, sign-in, watch state and redb are never opened.

Exit 0 = every expected log line was seen and the shot (if asked for) was written; 1 = an assertion
failed; 2 = usage, missing key or missing binary. See docs/nuvio/SIM-LOOP.md.
"""

import argparse
import json
import os
import shutil
import signal
import socket
import subprocess
import sys
import tempfile
import time
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
DEFAULT_BIN = REPO / "rust-modules/target-sim-nuvio/debug/plxnative-sim"
ADDONS_FILE = REPO / "tests/nuvio-public-addons.json"
DEBUG_MODULE = REPO / "rust-modules/src/app/nuvio/nuvio_debug.rs"
LOCAL_ENV = REPO / "rust-modules/stremio/nuvio.local.env"
FIXTURE_SERVER = REPO / "tests/serve_fixtures.py"
HOST_ENGINE = REPO / "build/sbengine-host/libsbengine.so"

LIVE_TRIGGER = "nuviolive"
ADDONS_TRIGGER = "nuvioaddons"
# Do NOT arm plxnative-discover here. account::discover() prefers local_discover() on EVERY call,
# and the fork's shim answers /library/people/{guid} and /credits from TMDB, so a person page's
# biography and filmography are real enrichment. A trigger would pin the base to one hard-coded
# port and break that path if the shim had to fall back to a free port.
LIVE_PROOFS = (
    "nuvio: LIVE tmdb=configured",
    "nuvio: LIVE addon installed:",
    "nuvio: shim serving on",
)


def say(message: str) -> None:
    print(message, flush=True)


def loop_key() -> str:
    """The dedicated loop key: env first, then the gitignored local env file. Never logged."""
    exported = os.environ.get("TMDB_API_KEY", "").strip()
    if exported:
        return exported
    if not LOCAL_ENV.is_file():
        return ""
    for line in LOCAL_ENV.read_text(encoding="utf-8", errors="replace").splitlines():
        stripped = line.strip()
        if stripped.startswith("export "):
            stripped = stripped[len("export "):].lstrip()
        if stripped.startswith("TMDB_API_KEY="):
            value = stripped.split("=", 1)[1].strip().strip("\"'")
            if value:
                return value
    return ""


def fail(message: str, code: int = 2):
    say(f"nuvio-sim: {message}")
    raise SystemExit(code)


def free_port() -> int:
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as sock:
        sock.bind(("127.0.0.1", 0))
        return int(sock.getsockname()[1])


def wait_port(port: int, timeout: float) -> None:
    end = time.monotonic() + timeout
    while time.monotonic() < end:
        try:
            with socket.create_connection(("127.0.0.1", port), timeout=0.5):
                return
        except OSError:
            time.sleep(0.1)
    fail(f"the fixture server never listened on 127.0.0.1:{port}", 1)


def arm(root: Path, name: str, value: str = "") -> Path:
    path = root / f"plxnative-{name}"
    path.write_text(value, encoding="utf-8")
    return path


def sanitize(tokens: str) -> list[str]:
    raw = tokens.replace(",", " ").split()
    for token in raw:
        if any(ch.isspace() for ch in token) or not token:
            fail(f"refusing token {token!r}: one token per whitespace-delimited word")
    return raw


def watch_log(path: Path, needles: list[str], timeout: float, proc: subprocess.Popen,
              what: str) -> tuple[str, list[str]]:
    seen: set[str] = set()
    text = ""
    end = time.monotonic() + timeout
    while time.monotonic() < end:
        if path.is_file():
            text = path.read_text(encoding="utf-8", errors="replace")
            for needle in needles:
                if needle in text:
                    seen.add(needle)
        if len(seen) == len(needles):
            return text, sorted(seen)
        if proc.poll() is not None:
            break
        time.sleep(0.2)
    missing = [n for n in needles if n not in seen]
    fail(f"{what}: log did not show {missing} within {timeout:g}s "
         f"(runtime log: {path}); app exit={proc.poll()}", 1)


def open_fifo(path: Path, timeout: float):
    end = time.monotonic() + timeout
    while time.monotonic() < end:
        if path.exists():
            return os.open(str(path), os.O_RDWR)
        time.sleep(0.1)
    fail(f"the app never created its remote FIFO at {path}", 1)


def drive(fifo_fd: int, tokens: list[str], gap: float) -> None:
    for token in tokens:
        os.write(fifo_fd, f"{token} ".encode())
        say(f"  key: {token}")
        time.sleep(gap)


def shot_from_log(text: str) -> Path | None:
    for line in text.splitlines():
        if line.startswith("shot: wrote ") and " to " in line:
            candidate = Path(line.split(" to ", 1)[1].strip())
            if candidate.is_file() and candidate.stat().st_size > 0:
                return candidate
    return None


def wait_for_shot(path: Path, text: str, timeout: float, proc: subprocess.Popen) -> Path:
    end = time.monotonic() + timeout
    while time.monotonic() < end:
        found = shot_from_log(text)
        if found:
            return found
        if path.is_file():
            text = path.read_text(encoding="utf-8", errors="replace")
        if proc.poll() is not None:
            break
        time.sleep(0.2)
    fail(f"no shot was written (asked for a PNG in {path.parent})", 1)


def run(args: argparse.Namespace) -> None:
    key = loop_key()
    if not key:
        fail(f"no dedicated TMDB key: export TMDB_API_KEY or put it in {LOCAL_ENV}. "
             "The production key is a GitHub secret and is deliberately not used here.")
    # The child inherits the caller's environment; set the resolved key explicitly so the
    # local-file path works too. The value is never echoed or written anywhere.
    env_key = key
    binary = Path(args.bin).resolve()
    if not binary.is_file():
        fail(f"simulator binary not found: {binary}. Build with `make sim-nuvio` "
             "(macOS) or `make sim-nuvio-linux` (Linux/WSLg).")

    runtime = Path(args.runtime).resolve() if args.runtime else Path(tempfile.mkdtemp(prefix="nuvio-sim-"))
    runtime.mkdir(parents=True, exist_ok=True)
    log = runtime / "plxnative-events.log"

    server = None
    expected = list(LIVE_PROOFS) + list(args.expect)
    if args.media:
        # A real playback check: serve a LOCAL media file through the repo's Range-capable
        # fixture server, arm the typed playurl declaration and the clock sink. Nothing here
        # touches a PMS, an account, a TV or a decoder.
        media = Path(args.media).resolve()
        if not media.is_file():
            fail(f"media file not found: {media}")
        port = free_port()
        server = subprocess.Popen(
            [sys.executable, str(FIXTURE_SERVER), "--root", str(media.parent), "--port", str(port)],
            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        wait_port(port, args.timeout)
        declaration = {"url": f"http://127.0.0.1:{port}/{media.name}",
                       "vcodec": args.vcodec, "acodec": args.acodec}
        arm(runtime, "playurl", json.dumps(declaration, separators=(",", ":")))
        arm(runtime, "clocksink")
        expected += ["clocksink: ARMED", "ff: v=#0 codec=", "route=player"]
        say(f"nuvio-sim: serving {media.name} at 127.0.0.1:{port}")

    # Seal the boot and point it at the tracked public addon list. Nothing here reads the
    # household's addon config, account or redb.
    arm(runtime, LIVE_TRIGGER)
    addon_file = Path(args.addons).resolve() if args.addons else ADDONS_FILE
    if not addon_file.is_file():
        fail(f"public addon list not found: {addon_file}")
    private_addons = runtime / "nuvio-public-addons.json"
    shutil.copyfile(addon_file, private_addons)
    arm(runtime, ADDONS_TRIGGER, str(private_addons))
    for extra in args.trigger:
        name, sep, value = extra.partition("=")
        if not name or any(c not in "abcdefghijklmnopqrstuvwxyz0123456789_-" for c in name):
            fail(f"bad trigger name {name!r}")
        arm(runtime, name, value if sep else "")

    tokens = sanitize(args.keys) if args.keys else []
    env = os.environ.copy()
    env["TMDB_API_KEY"] = env_key  # passed to the child only; never printed or persisted
    env["PLXNATIVE_RUNTIME_DIR"] = str(runtime)
    app_dir = REPO / "pkg"
    if args.host_engine:
        # The shim drops every torrent/magnet source when p2p is off, and p2p is off whenever
        # libsbengine.so cannot be opened from the app dir. A private app dir with symlinks to
        # pkg plus the HOST engine makes real addon torrent sources visible in the Streams
        # picker without overwriting the ARM library that `make deploy` ships.
        if not HOST_ENGINE.is_file():
            fail(f"host engine not found: {HOST_ENGINE}. Build it with tools/host-engine-check.sh --build-only")
        app_dir = runtime / "app"
        app_dir.mkdir(exist_ok=True)
        for entry in (REPO / "pkg").iterdir():
            link = app_dir / entry.name
            if not link.exists():
                link.symlink_to(entry.resolve())
        engine_link = app_dir / "libsbengine.so"
        if engine_link.is_symlink() or engine_link.exists():
            engine_link.unlink()
        engine_link.symlink_to(HOST_ENGINE.resolve())
        say(f"nuvio-sim: host engine staged in {app_dir} — torrent sources stay playable")
    env["PLXNATIVE_APP_DIR"] = str(app_dir)
    if args.shot:
        env["PLXNATIVE_SHOT"] = str(runtime / f"{args.shot}.png")

    say(f"nuvio-sim: boot {binary.name} in {runtime}")
    proc = subprocess.Popen([str(binary), "127.0.0.1", "32400"], env=env, cwd=str(REPO),
                            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    fifo_fd = None
    try:
        text, seen = watch_log(log, expected, args.timeout, proc, "boot")
        say(f"nuvio-sim: live boot proved ({len(seen)} lines): {', '.join(seen)}")
        if args.settle > 0:
            time.sleep(args.settle)
        if tokens or args.shot:
            fifo_fd = open_fifo(runtime / "plxnative-remote", args.timeout)
            if tokens:
                drive(fifo_fd, tokens, args.key_gap)
            if args.shot:
                os.write(fifo_fd, b"shot ")
                time.sleep(0.5)
                text = log.read_text(encoding="utf-8", errors="replace") if log.is_file() else text
                shot = wait_for_shot(log, text, args.timeout, proc)
                say(f"nuvio-sim: shot {shot}")
        # A key can change the screen; re-check expectations against the final log.
        final = log.read_text(encoding="utf-8", errors="replace") if log.is_file() else text
        missing = [n for n in expected if n not in final]
        if missing:
            fail(f"final log lost expected lines {missing}", 1)
    finally:
        if fifo_fd is not None:
            os.close(fifo_fd)
        if server is not None and server.poll() is None and not args.keep:
            server.terminate()
            try:
                server.wait(timeout=5)
            except subprocess.TimeoutExpired:
                server.kill()
                server.wait(timeout=5)
        if proc.poll() is None and not args.keep:
            proc.terminate()
            try:
                proc.wait(timeout=5)
            except subprocess.TimeoutExpired:
                proc.kill()
                proc.wait(timeout=5)
    if args.keep and proc.poll() is None:
        note = f"nuvio-sim: PASS — left app pid {proc.pid} running; stop with `kill {proc.pid}`"
        if server is not None and server.poll() is None:
            note += f" (fixture server pid {server.pid})"
        say(note)
    else:
        say(f"nuvio-sim: PASS (log: {log})")


def selftest() -> None:
    checks = 0
    if not ADDONS_FILE.is_file():
        fail(f"missing tracked addon list: {ADDONS_FILE}")
    data = json.loads(ADDONS_FILE.read_text(encoding="utf-8"))
    urls = data.get("addons")
    if not isinstance(urls, list) or not urls:
        fail("addon list has no 'addons' array")
    if not all(isinstance(u, str) and u.startswith("https://") for u in urls):
        fail("every public addon URL must be https")
    checks += 1
    source = DEBUG_MODULE.read_text(encoding="utf-8")
    for constant in (f'"{LIVE_TRIGGER}"', f'"{ADDONS_TRIGGER}"'):
        if constant not in source:
            fail(f"app module no longer declares {constant}: trigger wiring drifted")
    checks += 1
    assert sanitize("down, right ok") == ["down", "right", "ok"]
    checks += 1
    source = "environment" if os.environ.get("TMDB_API_KEY", "").strip() else (
        "gitignored local env file" if LOCAL_ENV.is_file() else "MISSING")
    say(f"nuvio-sim: selftest OK ({checks} checks, {len(urls)} public addons, "
        f"dedicated key source: {source}, no network)")


def parser() -> argparse.ArgumentParser:
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("mode", nargs="?", choices=["run", "selftest"], default="run",
                   help="`selftest` checks wiring offline; `run` boots the simulator")
    p.add_argument("--runtime", help="instance root (default: a fresh /tmp/nuvio-sim-*)")
    p.add_argument("--bin", default=str(DEFAULT_BIN), help="simulator binary")
    p.add_argument("--addons", help="public addon JSON (default: tests/nuvio-public-addons.json)")
    p.add_argument("--trigger", action="append", default=[], metavar="NAME[=VALUE]",
                   help="extra dev trigger in the instance root (repeatable)")
    p.add_argument("--keys", default="", help="tokens to send, e.g. 'down,down,right,ok'")
    p.add_argument("--key-gap", type=float, default=0.6, help="seconds between keys (default 0.6)")
    p.add_argument("--settle", type=float, default=8.0, help="seconds to let Home land (default 8)")
    p.add_argument("--shot", help="write a screenshot named <shot>-N.png in the instance root")
    p.add_argument("--expect", action="append", default=[], metavar="LINE",
                   help="event-log substring that must appear (repeatable)")
    p.add_argument("--timeout", type=float, default=90.0, help="boot/shot timeout in seconds")
    p.add_argument("--media", help="LOCAL H.264/AAC file to play with the clock sink (one command)")
    p.add_argument("--vcodec", default="h264", help="playurl video declaration (default h264)")
    p.add_argument("--acodec", default="aac", help="playurl audio declaration (default aac)")
    p.add_argument("--host-engine", action="store_true",
                   help="stage the host libsbengine.so in a private app dir so torrent sources show")
    p.add_argument("--keep", action="store_true",
                   help="leave the simulator running (prints its pid) for tools/a11y.py inspection")
    return p


if __name__ == "__main__":
    ns = parser().parse_args()
    if ns.mode == "selftest":
        selftest()
    else:
        run(ns)

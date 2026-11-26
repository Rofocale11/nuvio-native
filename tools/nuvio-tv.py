#!/usr/bin/env python3
"""Boot the LIVE Nuvio profile on the dev TV — the device twin of `nuvio-sim.py`.

This is NOT the host loop. It takes the repository TV lock, deploys `pkg/plxnative` and drives the
real set through `tools/tv-session.sh`. Run `make` first: the TV gets whatever ARM binary is in
`pkg/plxnative`, and the dedicated TMDB key is baked into it at build time from the gitignored
`rust-modules/stremio/nuvio.local.env`.

    make                                   # build the ARM binary
    python3 tools/nuvio-tv.py selftest     # offline wiring check, no TV

    # One-shot:
    python3 tools/nuvio-tv.py --shot home
    python3 tools/nuvio-tv.py --open-detail --shot detail

    # Persistent session — boot ONCE, then drive and shoot without relaunching:
    python3 tools/nuvio-tv.py start
    python3 tools/nuvio-tv.py shot home
    python3 tools/nuvio-tv.py detail       # read the live snapshot, press a card into Detail
    python3 tools/nuvio-tv.py shot detail
    python3 tools/nuvio-tv.py key down
    python3 tools/nuvio-tv.py state home.json
    python3 tools/nuvio-tv.py stop

The boot is `plxnative-nuviolive`: the same public Stremio addons and TMDB enrichment as the
simulator, but with a sealed dev-PMS identity and a private redb. No Nuvio or Plex account is
opened (`--no-token`), and no watch state is written. The `IN USE:` pre-flight is a refusal unless
the caller sets `PLX_TV_IN_USE_OK=1`. The lease is released automatically, including on Ctrl-C.

`--shot` tries the panel service, then falls back to the app's own capture stream, saving a JPEG
locally — nothing is written to the TV. `--open-detail` is the safe way to reach a real title:
numeric `ratingKey`s are rebuilt on every synthetic boot, so a key from a previous run 404s.
`--state FILE` and `--press ID [--rk RK]` expose the same by-id driving as the host loop's
`tools/a11y.py`. Host-only features (`--media`, `--host-engine`, `--keep`, `--bin`) are absent.

Exit 0 = every expected log line was seen; 1 = an assertion or device command failed;
2 = usage or preconditions. See docs/nuvio/TV-LOOP.md.
"""

import argparse
import json
import os
import socket
import subprocess
import sys
import tempfile
import time
import urllib.request
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
ADDONS_FILE = REPO / "tests/nuvio-public-addons.json"
DEBUG_MODULE = REPO / "rust-modules/src/app/nuvio/nuvio_debug.rs"
SESSION = REPO / "tools/tv-session.sh"
LOCK = REPO / "tools/tv-lock.sh"
SSH = REPO / "tools/tv-ssh"
MAKEFILE = REPO / "Makefile"

LIVE_TRIGGER = "nuviolive"
ADDONS_TRIGGER = "nuvioaddons"
# Must stay in step with tools/nuvio-sim.py and nuvio_debug.rs; `selftest` checks both.
LIVE_PROOFS = (
    "nuvio: LIVE tmdb=configured",
    "nuvio: LIVE addon installed:",
    "nuvio: shim serving on",
)
SESSION_FILE = Path(tempfile.gettempdir()) / "nuvio-tv-session.json"


def say(message: str) -> None:
    print(message, flush=True)


def fail(message: str, code: int = 2):
    say(f"nuvio-tv: {message}")
    raise SystemExit(code)


def call(cmd: list[str], *, check: bool = True, timeout: float | None = None) -> subprocess.CompletedProcess:
    result = subprocess.run(cmd, cwd=REPO, text=True, timeout=timeout,
                            stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    if check and result.returncode:
        fail(f"{' '.join(cmd)} failed ({result.returncode}):\n"
             f"{result.stdout}{result.stderr}".rstrip(), 1)
    return result


def sanitize(tokens: str) -> list[str]:
    raw = tokens.replace(",", " ").split()
    for token in raw:
        if not token or any(ch.isspace() for ch in token):
            fail(f"refusing token {token!r}: one token per whitespace-delimited word")
    return raw


def tv_log(flavor: str) -> str:
    return call([str(SESSION), "log", "--flavor", flavor]).stdout


def watch_tv_log(needles: list[str], timeout: float, flavor: str) -> tuple[str, list[str]]:
    """The event log is a file on the set, fetched over ssh — poll it rather than tailing it."""
    seen: set[str] = set()
    text = ""
    end = time.monotonic() + timeout
    while time.monotonic() < end:
        text = tv_log(flavor)
        for needle in needles:
            if needle in text:
                seen.add(needle)
        if len(seen) == len(needles):
            return text, sorted(seen)
        time.sleep(0.5)
    fail(f"TV log did not show {[n for n in needles if n not in seen]} within {timeout:g}s", 1)


def free_port() -> int:
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as sock:
        sock.bind(("127.0.0.1", 0))
        return int(sock.getsockname()[1])


def capture_shot(flavor: str, rundir: str, runtime: Path, name: str) -> Path | None:
    """Panel service first; where it is absent, use the app's own capture stream.

    Both write locally — the app-stream path never puts a file on the television.
    """
    panel = runtime / f"{name}.png"
    result = call([str(SESSION), "shot", "--flavor", flavor, str(panel)], check=False, timeout=30)
    if result.returncode == 0 and panel.is_file() and panel.stat().st_size:
        say(f"nuvio-tv: panel shot {panel}")
        return panel

    jpg = runtime / f"{name}.jpg"
    port = free_port()
    app_port = call(["make", "-s", "print-appport", f"FLAVOR={flavor}"]).stdout.strip()
    stream_log = runtime / f"{name}-stream.log"
    stream_out = stream_log.open("w", encoding="utf-8")
    streamer = subprocess.Popen(
        [sys.executable, "-u", str(REPO / "tools/stream-screen.py"),
         "--port", str(port), "--codec", "jpeg", "--source", "app",
         "--runtime-dir", rundir, "--app-port", app_port],
        cwd=REPO, stdout=stream_out, stderr=subprocess.STDOUT)
    try:
        time.sleep(6)
        for _ in range(3):
            try:
                urllib.request.urlretrieve(f"http://127.0.0.1:{port}/frame.jpg", jpg)
            except OSError:
                pass
            if jpg.is_file() and jpg.stat().st_size > 1000:
                break
            time.sleep(2)
    finally:
        streamer.terminate()
        try:
            streamer.wait(timeout=5)
        except subprocess.TimeoutExpired:
            streamer.kill()
            streamer.wait(timeout=5)
        stream_out.close()
    if jpg.is_file() and jpg.stat().st_size > 1000:
        say(f"nuvio-tv: app-stream shot {jpg} (UI plane only)")
        return jpg
    say("nuvio-tv: no frame from either capture path; event log is the evidence")
    return None


def pick_detail(flavor: str, runtime: Path) -> tuple[str, str]:
    """Read the live Home snapshot and open a title by id, the same way the host loop does."""
    out = runtime / "a11y.json"
    call([str(SESSION), "state", "--flavor", flavor, "--out", str(out)], timeout=30)
    elements = json.loads(out.read_text(encoding="utf-8")).get("elements", [])
    card = next((e for e in elements
                 if e.get("id", "").startswith("home.shelf.") and e.get("subject")), None)
    if card is None:
        fail("no Home shelf card with a subject in the live snapshot", 1)
    ident, rk = card["id"], card["subject"]
    call([str(SESSION), "press", "--flavor", flavor, ident, "--rk", rk], timeout=30)
    say(f"  opened {ident} rk={rk}")
    return ident, rk


def save_session(data: dict) -> None:
    SESSION_FILE.write_text(json.dumps(data), encoding="utf-8")
    os.chmod(SESSION_FILE, 0o600)


def load_session() -> dict:
    if not SESSION_FILE.is_file():
        fail("no session — run `nuvio-tv.py start` first", 2)
    return json.loads(SESSION_FILE.read_text(encoding="utf-8"))


def session_start(args: argparse.Namespace) -> None:
    """Boot once, leave the app up, and hold one lease for the whole interactive session."""
    if SESSION_FILE.is_file():
        fail("a session is already recorded; `nuvio-tv.py stop` it first", 2)
    if args.flavor != "debug":
        fail("nuviolive needs devtriggers, which only the debug install has; use --flavor debug", 2)
    if not (REPO / "pkg/plxnative").is_file():
        fail("pkg/plxnative missing: build the ARM binary with `make` first", 2)

    acquired = call([str(LOCK), "acquire", "--ttl", "30", "--why", "nuvio-tv session"],
                    check=False)
    if acquired.returncode:
        fail(f"could not take the TV lock:\n{acquired.stdout}{acquired.stderr}".rstrip(), 1)
    say(acquired.stdout.strip())
    if "IN USE:" in acquired.stdout and not os.environ.get("PLX_TV_IN_USE_OK"):
        call([str(LOCK), "release"], check=False)
        fail("pre-flight said IN USE; set PLX_TV_IN_USE_OK=1 to override", 1)

    runtime = Path(args.runtime).resolve() if args.runtime else Path(tempfile.mkdtemp(prefix="nuvio-tv-session-"))
    runtime.mkdir(parents=True, exist_ok=True)
    flavor = args.flavor
    rundir = call(["make", "-s", "print-rundir", f"FLAVOR={flavor}"]).stdout.strip()
    app_port = call(["make", "-s", "print-appport", f"FLAVOR={flavor}"]).stdout.strip()
    if not rundir or not app_port:
        call([str(LOCK), "release"], check=False)
        fail("could not resolve the install's runtime root or capture port", 1)
    addon_file = Path(args.addons).resolve() if args.addons else ADDONS_FILE
    if not addon_file.is_file():
        call([str(LOCK), "release"], check=False)
        fail(f"public addon list not found: {addon_file}")

    call([str(SSH), "ssh", "tv", f"mkdir -p {rundir}"])
    remote_addons = f"{rundir}/nuvio-public-addons.json"
    call([str(SSH), "scp", str(addon_file), f"tv:{remote_addons}"])

    up = [str(SESSION), "up", "--flavor", flavor, "--screen", args.screen, "--no-token",
          "--arm", LIVE_TRIGGER, "--arm", f"nuvioaddons={remote_addons}",
          "--arm", f"capture={app_port}"]
    for extra in args.trigger:
        name, sep, value = extra.partition("=")
        if not name or any(c not in "abcdefghijklmnopqrstuvwxyz0123456789_-" for c in name):
            fail(f"bad trigger name {name!r}")
        up += ["--arm", f"{name}={value}" if sep else name]
    result = call(up, check=False, timeout=max(args.timeout, 180))
    if result.returncode:
        call([str(LOCK), "release"], check=False)
        fail(f"TV session did not come up:\n{result.stdout}{result.stderr}".rstrip(), 1)
    say(result.stdout.strip())
    try:
        watch_tv_log(LIVE_PROOFS, args.timeout, flavor)
    except SystemExit:
        call([str(LOCK), "release"], check=False)
        raise

    save_session({"flavor": flavor, "rundir": rundir, "app_port": app_port,
                  "runtime": str(runtime), "screen": args.screen})
    say(f"nuvio-tv: session ready — app is up on {flavor}, lease held. "
        f"Use `shot`, `key`, `state`, `press`, `detail`, `log`, then `stop`.")


def session_action(verb: str, args: argparse.Namespace) -> None:
    data = load_session()
    call([str(LOCK), "require", "--quiet", "--why", f"nuvio-tv {verb}"], check=False)
    flavor, rundir = data["flavor"], data["rundir"]
    runtime = Path(data["runtime"])

    if verb == "shot":
        name = args.rest[0] if args.rest else "shot"
        capture_shot(flavor, rundir, runtime, name)
    elif verb == "key":
        for token in sanitize(" ".join(args.rest)):
            result = call([str(SESSION), "key", "--flavor", flavor, token], check=False)
            if result.returncode:
                fail(f"TV key {token!r} refused:\n{result.stdout}{result.stderr}".rstrip(), 1)
            say(f"  key: {token}")
    elif verb == "state":
        out = Path(args.rest[0]) if args.rest else runtime / "a11y.json"
        call([str(SESSION), "state", "--flavor", flavor, "--out", str(out)], timeout=30)
        say(f"nuvio-tv: state {out}")
    elif verb == "press":
        rest = list(args.rest)
        rk = args.rk
        if "--rk" in rest:  # REMAINDER keeps this option for us
            at = rest.index("--rk")
            rk = rest[at + 1] if at + 1 < len(rest) else ""
            del rest[at:at + 2]
        if not rest:
            fail("press needs an accessibility id", 2)
        press = [str(SESSION), "press", "--flavor", flavor, rest[0]]
        if rk:
            press += ["--rk", rk]
        result = call(press, check=False, timeout=30)
        if result.returncode:
            fail(f"TV press {rest[0]!r} refused:\n{result.stdout}{result.stderr}".rstrip(), 1)
        say(f"  pressed {rest[0]}" + (f" rk={rk}" if rk else ""))
    elif verb == "detail":
        _, rk = pick_detail(flavor, runtime)
        watch_tv_log([f"rk={rk} show="], args.timeout, flavor)
        say(f"nuvio-tv: Detail open rk={rk}")
    elif verb == "log":
        pattern = args.rest[0] if args.rest else ""
        lines = [line for line in tv_log(flavor).splitlines() if not pattern or pattern in line]
        say("\n".join(lines[-200:]))
    elif verb == "status":
        say(json.dumps(data, indent=2))
        say(call([str(SESSION), "status", "--flavor", flavor], check=False).stdout.strip())


def session_stop() -> None:
    data = load_session()
    call([str(LOCK), "require", "--quiet", "--why", "nuvio-tv stop"], check=False)
    down = call([str(SESSION), "down", "--flavor", data["flavor"]], check=False)
    release = call([str(LOCK), "release"], check=False)
    SESSION_FILE.unlink(missing_ok=True)
    if down.returncode or release.returncode:
        say("nuvio-tv: WARNING — down or lease release failed; hand the TV back yourself")
    else:
        say("nuvio-tv: session stopped, app handed back, lease released")


def run(args: argparse.Namespace) -> None:
    if args.flavor != "debug":
        fail("nuviolive needs devtriggers, which only the debug install has; use --flavor debug", 2)
    if not (REPO / "pkg/plxnative").is_file():
        fail("pkg/plxnative missing: build the ARM binary with `make` first", 2)

    runtime = Path(args.runtime).resolve() if args.runtime else Path(tempfile.mkdtemp(prefix="nuvio-tv-"))
    runtime.mkdir(parents=True, exist_ok=True)
    log = runtime / "plxnative-events.log"

    flavor = args.flavor
    rundir = call(["make", "-s", "print-rundir", f"FLAVOR={flavor}"]).stdout.strip()
    if not rundir:
        fail("could not resolve the install's runtime root", 1)
    addon_file = Path(args.addons).resolve() if args.addons else ADDONS_FILE
    if not addon_file.is_file():
        fail(f"public addon list not found: {addon_file}")

    # Stage the tracked addon list where the jailed app can read it. `up` clears plxnative-*
    # triggers before launch but does not remove this file.
    call([str(SSH), "ssh", "tv", f"mkdir -p {rundir}"])
    remote_addons = f"{rundir}/nuvio-public-addons.json"
    call([str(SSH), "scp", str(addon_file), f"tv:{remote_addons}"])

    arm_args = [f"nuvioaddons={remote_addons}"]
    for extra in args.trigger:
        name, sep, value = extra.partition("=")
        if not name or any(c not in "abcdefghijklmnopqrstuvwxyz0123456789_-" for c in name):
            fail(f"bad trigger name {name!r}")
        arm_args.append(f"{name}={value}" if sep else name)

    app_port = call(["make", "-s", "print-appport", f"FLAVOR={flavor}"]).stdout.strip()
    if args.shot:
        # The app only opens its capture listener when this boot trigger is present.
        arm_args.append(f"capture={app_port}")

    expected = list(LIVE_PROOFS) + list(args.expect)
    up = [str(SESSION), "up", "--flavor", flavor, "--screen", args.screen,
          "--no-token", "--arm", LIVE_TRIGGER]
    for arm_arg in arm_args:
        up += ["--arm", arm_arg]
    tokens = sanitize(args.keys) if args.keys else []
    if args.open_detail and args.press:
        fail("--open-detail and --press are exclusive", 2)
    if args.open_detail and not args.screen.startswith("home"):
        fail("--open-detail boots Home first; leave --screen at its default", 2)

    say(f"nuvio-tv: boot {flavor} → {args.screen} (local log: {log})")
    try:
        result = call(up, check=False, timeout=max(args.timeout, 180))
        if result.returncode:
            fail(f"TV session did not come up:\n{result.stdout}{result.stderr}".rstrip(), 1)
        say(result.stdout.strip())

        text, seen = watch_tv_log(expected, args.timeout, flavor)
        say(f"nuvio-tv: live boot proved ({len(seen)} lines): {', '.join(seen)}")
        log.write_text(text, encoding="utf-8")
        if args.screen.startswith("detail=") and "no metadata" in text:
            fail("that ratingKey does not resolve in this boot: every synthetic live boot "
                 "rebuilds its key table. Use --open-detail to pick a title in the same boot.", 1)

        if args.settle > 0:
            time.sleep(args.settle)
        for token in tokens:
            result = call([str(SESSION), "key", "--flavor", flavor, token], check=False)
            if result.returncode:
                fail(f"TV key {token!r} refused:\n{result.stdout}{result.stderr}".rstrip(), 1)
            say(f"  key: {token}")
            time.sleep(args.key_gap)

        if args.open_detail:
            _, rk = pick_detail(flavor, runtime)
            # A by-id press does NOT log `plxnative-detail:` — that line belongs to the boot
            # trigger only. The page's own line is `detail: sid=0 rk=<rk> show=...`.
            marker = f"rk={rk} show="
            watch_tv_log([marker], args.timeout, flavor)
            expected.append(marker)
        if args.state:
            out = Path(args.state)
            out = runtime / out if not out.is_absolute() else out
            call([str(SESSION), "state", "--flavor", flavor, "--out", str(out)], timeout=30)
            say(f"nuvio-tv: state {out}")
        if args.press:
            press = [str(SESSION), "press", "--flavor", flavor, args.press]
            if args.rk:
                press += ["--rk", args.rk]
            result = call(press, check=False, timeout=30)
            if result.returncode:
                fail(f"TV press {args.press!r} refused:\n{result.stdout}{result.stderr}".rstrip(), 1)
            say(f"  pressed {args.press}" + (f" rk={args.rk}" if args.rk else ""))

        if args.shot:
            capture_shot(flavor, rundir, runtime, args.shot)

        final = tv_log(flavor)
        missing = [n for n in expected if n not in final]
        if missing:
            fail(f"final TV log lost expected lines {missing}", 1)
        log.write_text(final, encoding="utf-8")
    finally:
        down = call([str(SESSION), "down", "--flavor", flavor], check=False)
        if down.returncode:
            say("nuvio-tv: WARNING — `down` did not complete; hand the TV back yourself")
    say(f"nuvio-tv: PASS (log: {log})")


def selftest() -> None:
    """Offline wiring check: no ssh, no lock, no television."""
    if not ADDONS_FILE.is_file():
        fail(f"missing tracked addon list: {ADDONS_FILE}")
    urls = json.loads(ADDONS_FILE.read_text(encoding="utf-8")).get("addons")
    if not isinstance(urls, list) or not urls or not all(
            isinstance(u, str) and u.startswith("https://") for u in urls):
        fail("the addon list must be a non-empty array of https URLs")
    source = DEBUG_MODULE.read_text(encoding="utf-8")
    for constant in (f'"{LIVE_TRIGGER}"', f'"{ADDONS_TRIGGER}"'):
        if constant not in source:
            fail(f"app module no longer declares {constant}: trigger wiring drifted")
    for needle in LIVE_PROOFS:
        if needle not in (REPO / "tools/nuvio-sim.py").read_text(encoding="utf-8"):
            fail(f"live proof drifted from the host loop: {needle}")
    for tool, flag in ((SESSION, "--arm"), (SESSION, "--no-token"), (SESSION, "setpriv"),
                       (LOCK, "with"), (SSH, "scp"),
                       (REPO / "tools/stream-screen.py", "--codec")):
        if not tool.is_file() or flag not in tool.read_text(encoding="utf-8", errors="replace"):
            fail(f"{tool.name} no longer supports {flag}: TV wiring drifted")
    makefile = MAKEFILE.read_text(encoding="utf-8", errors="replace")
    if "TMDB_API_KEY" not in makefile or "nuvio.local.env" not in makefile:
        fail("the ARM build no longer bakes in the dedicated TMDB key")
    assert sanitize("down, right ok") == ["down", "right", "ok"]
    say(f"nuvio-tv: selftest OK (offline, {len(urls)} public addons)")


def parser() -> argparse.ArgumentParser:
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("mode", nargs="?", default="run",
                   choices=["run", "selftest", "start", "shot", "key", "state", "press",
                            "detail", "log", "status", "stop"],
                   help="`run` is one shot; `start` opens a session, then shot/key/state/press/"
                        "detail/log/status act on that same app until `stop`")
    p.add_argument("rest", nargs=argparse.REMAINDER, help=argparse.SUPPRESS)
    p.add_argument("--runtime", help="local directory for the downloaded log and shot")
    p.add_argument("--addons", help="public addon JSON (default: tests/nuvio-public-addons.json)")
    p.add_argument("--trigger", action="append", default=[], metavar="NAME[=VALUE]",
                   help="extra dev trigger to arm at boot (repeatable)")
    p.add_argument("--keys", default="", help="tokens to send, e.g. 'down,down,right,ok'")
    p.add_argument("--key-gap", type=float, default=0.6, help="seconds between keys (default 0.6)")
    p.add_argument("--settle", type=float, default=8.0, help="seconds to let the screen land")
    p.add_argument("--shot", help="download a panel capture named <shot>-N.png")
    p.add_argument("--expect", action="append", default=[], metavar="LINE",
                   help="event-log substring that must appear (repeatable)")
    p.add_argument("--timeout", type=float, default=90.0, help="log proof timeout in seconds")
    p.add_argument("--screen", default="home",
                   help="boot screen, e.g. home or detail=<ratingKey> (default: home)")
    p.add_argument("--flavor", default="debug", help="TV install flavour (default: debug)")
    p.add_argument("--open-detail", action="store_true",
                   help="boot Home, read the live snapshot, press a shelf card into Detail")
    p.add_argument("--state", metavar="FILE",
                   help="write the a11y snapshot to FILE (by-id driving, as in the host loop)")
    p.add_argument("--press", metavar="ID",
                   help="press an accessibility id; pair with --rk for title-bearing elements")
    p.add_argument("--rk", metavar="ratingKey", help="title this --press must act on")
    return p


if __name__ == "__main__":
    ns = parser().parse_args()
    if ns.mode == "selftest":
        selftest()
    elif ns.mode == "start":
        session_start(ns)
    elif ns.mode == "stop":
        session_stop()
    elif ns.mode in ("shot", "key", "state", "press", "detail", "log", "status"):
        session_action(ns.mode, ns)
    elif os.environ.get("NUVIOTV_INNER") != "1":
        # Re-enter inside `with` so the lease is released on Ctrl-C, SIGTERM and a crash too.
        env = os.environ.copy()
        env["NUVIOTV_INNER"] = "1"
        raise SystemExit(subprocess.run(
            [str(LOCK), "with", "--ttl", "30", "--why", "nuvio-tv", "--",
             sys.executable, str(Path(__file__).resolve()), *sys.argv[1:]],
            cwd=REPO, env=env).returncode)
    else:
        run(ns)

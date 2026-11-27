#!/usr/bin/env python3
"""Boot the LIVE Nuvio profile on the dev TV — the device twin of `nuvio-sim.py`.

This is NOT the host loop. It takes the repository TV lock and drives the real set through
`tools/tv-session.sh`. It NEVER deploys: every boot path refuses when the deployed binary differs
from `pkg/plxnative`, because the owner requires approval for each deploy. Build with `make` first
(and `make FLAVOR=debug [ARM_PROFILE=release] deploy` only with that approval). The dedicated TMDB
key is baked into the binary at build time from the gitignored
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

    # Graded frame timing in one sealed boot (debug install only, panel/sound off):
    python3 tools/nuvio-tv.py scenes
    python3 tools/nuvio-tv.py bench --scene home-nav --shot home-nav
    python3 tools/nuvio-tv.py gate --scene home-nav-deep   # cold boot, then warm boot (release gate)

The boot is `plxnative-nuviolive`: the same public Stremio addons and TMDB enrichment as the
simulator, but with a sealed dev-PMS identity and a private redb. No Nuvio or Plex account is
opened (`--no-token`), and no watch state is written. The `IN USE:` pre-flight is a refusal unless
the OWNER sets `PLX_TV_IN_USE_OK=1`; an agent must not grant itself that override. The lease is
released automatically, including on Ctrl-C.

`--shot` tries the panel service, then falls back to the app's own capture stream, saving a JPEG
locally — nothing is written to the TV. `--open-detail` is the safe way to reach a real title:
numeric `ratingKey`s are rebuilt on every synthetic boot, so a key from a previous run 404s.
`--state FILE` and `--press ID [--rk RK]` expose the same by-id driving as the host loop's
`tools/a11y.py`. Host-only features (`--media`, `--host-engine`, `--keep`, `--bin`) are absent.

Exit 0 = every expected log line was seen; 1 = an assertion or device command failed;
2 = usage or preconditions. See docs/nuvio/TV-LOOP.md.
"""

import argparse
import hashlib
import importlib
import json
import os
import re
import shlex
import socket
import subprocess
import sys
import tempfile
import time
import urllib.request
from pathlib import Path

import arm_profile_guard  # fork-owned: timing scenes refuse a tvdev binary (tests/run.py's rule)

REPO = Path(__file__).resolve().parent.parent
ADDONS_FILE = REPO / "tests/nuvio-public-addons.json"
BENCH_ADDONS_FILE = REPO / "tests/nuvio-tv-benchmark-addons.json"
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
SCENES_FILE = REPO / "tests/nuvio-tv-scenes.json"
MODES = ("run", "selftest", "start", "shot", "key", "state", "press", "detail", "log",
         "expect", "status", "stop", "bench", "gate", "scenes")
VALUE_OPTS = frozenset({"--runtime", "--addons", "--trigger", "--keys", "--key-gap", "--settle",
                        "--shot", "--expect", "--timeout", "--screen", "--flavor", "--state",
                        "--press", "--rk", "--scene", "--runs"})
FLAG_OPTS = frozenset({"--open-detail", "--suite", "--panel-on"})


def say(message: str) -> None:
    print(message, flush=True)


def fail(message: str, code: int = 2):
    say(f"nuvio-tv: {message}")
    raise SystemExit(code)


def warn_control(verb: str, result: subprocess.CompletedProcess) -> None:
    """Some firmware builds refuse setMuted/turnOffScreen; never block a run on that."""
    detail = (result.stdout + result.stderr).strip().splitlines()
    say(f"nuvio-tv: WARNING — tv-session.sh {verb} off unavailable on this firmware: "
        f"{detail[-1] if detail else 'no detail'}; continuing")


def call(cmd: list[str], *, check: bool = True, timeout: float | None = None) -> subprocess.CompletedProcess:
    result = subprocess.run(cmd, cwd=REPO, text=True, timeout=timeout,
                            stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    if check and result.returncode:
        fail(f"{' '.join(cmd)} failed ({result.returncode}):\n"
             f"{result.stdout}{result.stderr}".rstrip(), 1)
    return result


def normalize_argv(argv: list[str]) -> list[str]:
    """Hoist options written AFTER the verb ahead of argparse's REMAINDER.

    `key down ok` and `press ID --rk RK` need REMAINDER, but that also swallows `--scene X` in
    `bench --scene X`. Move only known option tokens (and their values) before the verb; leave the
    verb's own tokens alone. Idempotent: the lease wrapper re-runs this with the same argv.
    """
    index, i = None, 0
    while i < len(argv):
        if argv[i] in VALUE_OPTS and "=" not in argv[i]:
            i += 2  # the value of an option is not the verb
            continue
        if argv[i] in MODES:
            index = i
            break
        i += 1
    if index is None:
        return argv
    head, verb, tail = argv[:index], argv[index], argv[index + 1:]
    options, tokens = [], []
    i = 0
    while i < len(tail):
        arg = tail[i]
        name = arg.split("=", 1)[0]
        if name in VALUE_OPTS:
            options.append(arg)
            if "=" not in arg and i + 1 < len(tail):
                i += 1
                options.append(tail[i])
        elif name in FLAG_OPTS:
            options.append(arg)
        else:
            tokens.append(arg)
        i += 1
    return head + options + [verb] + tokens


def make_query(name: str, flavor: str) -> str:
    """Ask the Makefile for one install fact; never restate its paths in this tool."""
    return call(["make", "-s", f"FLAVOR={flavor}", name]).stdout.strip()


APPID_RE = re.compile(r"^[a-z0-9][a-z0-9._-]*$")


def cold_cache_reset(flavor: str) -> str:
    """Delete the DEBUG install's image cache and nuvio.redb so the next boot is purely cold.

    Fail closed: only a debug app id built from `make print-appid` is ever named, and each candidate
    mirrors `plx_base::paths::{image_cache_candidates,nuvio_data_dir}` for that one id. The stable
    install is never a candidate. Returns a short list of what was actually removed.
    """
    appid = make_query("print-appid", flavor)
    if flavor != "debug" or not appid.endswith(".debug") or not APPID_RE.fullmatch(appid):
        fail("cold cache reset refused: only the debug install may be cleared", 2)
    candidates = [
        f"/media/developer/{appid}-imgcache",
        f"/media/internal/.{appid}-imgcache",
        f"/media/developer/temp/{appid}-imgcache",
        f"/media/developer/{appid}.nuvio/shim/nuvio.redb",
        f"/media/internal/.{appid}/nuvio/shim/nuvio.redb",
    ]
    quoted = " ".join(shlex.quote(p) for p in candidates)
    probe = "; ".join(f"[ -e {shlex.quote(p)} ] && echo {shlex.quote(p)}" for p in candidates)
    found = call([str(SSH), "ssh", "tv", probe], check=False).stdout.split()
    if found:
        result = call([str(SSH), "ssh", "tv", f"rm -rf -- {quoted}"], check=False)
        if result.returncode:
            fail(f"cold cache reset refused by the set:\n{result.stdout}{result.stderr}".rstrip(), 1)
    return ", ".join(found) if found else "nothing to clear"


def deployed_matches(flavor: str) -> tuple[bool, str]:
    """Compare pkg/plxnative with the bytes on the set — READ-ONLY, never a deploy.

    `tv-session.sh up` deploys when they differ. An unattended agent must not inherit that
    behaviour: the owner asked that every deploy need their approval, so a mismatch is a refusal.
    """
    binary = REPO / "pkg/plxnative"
    if not binary.is_file():
        return False, "pkg/plxnative is missing (build it first)"
    try:
        local = hashlib.md5(binary.read_bytes()).hexdigest()
    except OSError as exc:
        return False, f"cannot hash pkg/plxnative: {exc}"
    appdir = make_query("print-appdir", flavor)
    if not appdir:
        return False, "cannot resolve the install directory from the Makefile"
    remote = call([str(SSH), "ssh", "tv", f"md5sum {appdir}/plxnative"],
                  check=False, timeout=30)
    if remote.returncode:
        return False, "cannot read the deployed binary hash (is the set awake?)"
    parts = remote.stdout.split()
    if not parts or len(parts[0]) != 32:
        return False, "deployed binary hash is unreadable"
    return local == parts[0].lower(), ("byte-identical" if local == parts[0].lower()
                                       else f"local {local} != deployed {parts[0].lower()}")


def require_deployed_binary(flavor: str) -> None:
    ok, why = deployed_matches(flavor)
    if not ok:
        fail(f"deployed {flavor} binary does not match pkg/plxnative ({why}). "
             "nuvio-tv never deploys on its own: get the owner's approval, then run "
             f"`make FLAVOR={flavor} deploy` (add ARM_PROFILE=release for a timing run)", 2)


def load_scenes() -> dict[str, dict]:
    if not SCENES_FILE.is_file():
        fail(f"missing bench scene file: {SCENES_FILE}", 2)
    data = json.loads(SCENES_FILE.read_text(encoding="utf-8"))
    scenes = data.get("scenes") if isinstance(data, dict) else None
    if not isinstance(scenes, list) or not scenes:
        fail(f"{SCENES_FILE} must contain a non-empty `scenes` array", 2)
    out: dict[str, dict] = {}
    for scene in scenes:
        if not isinstance(scene, dict) or not scene.get("name") or not scene.get("route"):
            fail("every bench scene needs a `name` and a `route`", 2)
        name = scene["name"]
        if name in out:
            fail(f"duplicate bench scene {name!r}", 2)
        keys = scene.get("keys", [])
        if not isinstance(keys, list) or not all(isinstance(k, str) for k in keys):
            fail(f"bench scene {name}: `keys` must be a list of D-pad tokens", 2)
        for field in ("expect", "triggers"):
            if field in scene and (not isinstance(scene[field], list)
                                   or not all(isinstance(v, str) for v in scene[field])):
                fail(f"bench scene {name}: `{field}` must be a list of strings", 2)
        if "pause_after" in scene and (isinstance(scene["pause_after"], bool)
                                       or not isinstance(scene["pause_after"], int)
                                       or scene["pause_after"] < 1):
            fail(f"bench scene {name}: `pause_after` must be a positive key count", 2)
        if "pause_s" in scene and (isinstance(scene["pause_s"], bool)
                                   or not isinstance(scene["pause_s"], (int, float))):
            fail(f"bench scene {name}: `pause_s` must be seconds", 2)
        if "addons" in scene and not isinstance(scene["addons"], str):
            fail(f"bench scene {name}: `addons` must be a path", 2)
        out[name] = scene
    return out


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
    flavor = args.flavor
    if flavor != "debug":
        fail("nuviolive needs devtriggers, which only the debug install has; use --flavor debug", 2)
    require_deployed_binary(flavor)

    acquired = call([str(LOCK), "acquire", "--ttl", "30", "--why", "nuvio-tv session"],
                    check=False)
    if acquired.returncode:
        fail(f"could not take the TV lock:\n{acquired.stdout}{acquired.stderr}".rstrip(), 1)
    say(acquired.stdout.strip())
    if "IN USE:" in acquired.stdout and not os.environ.get("PLX_TV_IN_USE_OK"):
        call([str(LOCK), "release"], check=False)
        fail("pre-flight said IN USE: stop and tell the owner; only they may set "
             "PLX_TV_IN_USE_OK=1", 1)

    runtime = Path(args.runtime).resolve() if args.runtime else Path(tempfile.mkdtemp(prefix="nuvio-tv-session-"))
    runtime.mkdir(parents=True, exist_ok=True)
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
    # Owner's standing sound rule applies to every device run. The PANEL is left as the owner set
    # it so they can watch an interactive session; only `bench` blanks it.
    quiet = call([str(SESSION), "sound", "off", "--flavor", flavor], check=False)
    if quiet.returncode:
        warn_control("sound", quiet)

    save_session({"flavor": flavor, "rundir": rundir, "app_port": app_port,
                  "runtime": str(runtime), "screen": args.screen})
    say(f"nuvio-tv: session ready — app is up on {flavor}, lease held. "
        f"Use `shot`, `key`, `state`, `press`, `detail`, `log`, then `stop`.")


def session_action(verb: str, args: argparse.Namespace) -> None:
    data = load_session()
    # Fail CLOSED: `require` exits 1 when another lane holds the set. Driving on anyway is how two
    # jobs produce plausible WRONG data that reads like a regression.
    call([str(LOCK), "require", "--quiet", "--why", f"nuvio-tv {verb}"])
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
        text = tv_log(flavor)
        # Retain the evidence locally: a later boot overwrites the event log on the set.
        snapshot = runtime / "event-log.txt"
        snapshot.write_text(text, encoding="utf-8")
        lines = [line for line in text.splitlines() if not pattern or pattern in line]
        say("\n".join(lines[-200:]))
        say(f"nuvio-tv: full log saved to {snapshot}")
    elif verb == "expect":
        needles = list(args.rest)
        if not needles:
            fail("expect needs at least one log substring", 2)
        watch_tv_log(needles, args.timeout, flavor)
        say(f"nuvio-tv: {len(needles)} expected line(s) present")
    elif verb == "status":
        say(json.dumps(data, indent=2))
        say(call([str(SESSION), "status", "--flavor", flavor], check=False).stdout.strip())


def session_stop() -> None:
    data = load_session()
    call([str(LOCK), "require", "--quiet", "--why", "nuvio-tv stop"])
    down = call([str(SESSION), "down", "--flavor", data["flavor"]], check=False)
    release = call([str(LOCK), "release"], check=False)
    SESSION_FILE.unlink(missing_ok=True)
    if down.returncode or release.returncode:
        say("nuvio-tv: WARNING — down or lease release failed; hand the TV back yourself")
    else:
        say("nuvio-tv: session stopped, app handed back, lease released")


def run(args: argparse.Namespace) -> None:
    flavor = args.flavor
    if flavor != "debug":
        fail("nuviolive needs devtriggers, which only the debug install has; use --flavor debug", 2)
    require_deployed_binary(flavor)

    runtime = Path(args.runtime).resolve() if args.runtime else Path(tempfile.mkdtemp(prefix="nuvio-tv-"))
    runtime.mkdir(parents=True, exist_ok=True)
    log = runtime / "plxnative-events.log"

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
        quiet = call([str(SESSION), "sound", "off", "--flavor", flavor], check=False)
        if quiet.returncode:
            warn_control("sound", quiet)
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


def fps_module():
    """`tests/run.py` owns the device-calibrated heartbeat parser and gates; reuse, don't copy."""
    sys.path.insert(0, str(REPO / "tests"))
    return importlib.import_module("run")


def grade_scene(scene: dict, lines: list[str], harness) -> tuple[bool, str]:
    """Grade one scene's `loop=`/`fps=` windows with the upstream harness's own gates."""
    route, overlay = scene["route"], scene.get("overlay")
    warmup = int(scene.get("warmup_s", 5))
    stats = harness.rate_stats(harness.parse_loop(lines, route, overlay)[warmup:])
    details = [f"route={route} loop samples={stats['n']}, median={stats['median']}, "
               f"robust_min={stats['robust_min']}, drift={stats['drift']:+.1f}"]
    if stats["n"] < 5:
        return False, " | ".join(details + ["need >= 5 post-warmup loop= samples "
                                            "(the scene never entered this route?)"])
    ok = True
    floor = scene.get("loop_floor")
    if floor is not None:
        hit = stats["robust_min"] >= floor
        ok = ok and hit
        details.append(f"loop_floor {floor}: {'PASS' if hit else 'FAIL'}")
    fps = harness.parse_fps(lines, route, overlay)[warmup:]
    f_floor = scene.get("fps_floor")
    if f_floor is not None:
        if len(fps) < 5:
            ok, note = False, " | fewer than 5 post-warmup fps= samples"
        else:
            hit, note = harness.fps_floor_verdict(
                fps, f_floor, scene.get("fps_floor_stat", "median"),
                scene.get("fps_floor_windows"))
            ok = ok and hit
            note = f" (fps_floor {f_floor}: {'PASS' if hit else 'FAIL'}{note})"
        details.append(note.lstrip(" |"))
    else:
        details.append(f"fps median={harness.rate_stats(fps)['median'] if fps else 'n/a'} "
                       "(reported, not gated)")
    ceiling = scene.get("fps_ceiling")
    if ceiling is not None:
        if len(fps) < 5:
            ok, note = False, " | fewer than 5 post-warmup fps= samples for fps_ceiling"
        else:
            ordered = sorted(fps, reverse=True)
            robust_max = ordered[1]
            hit = robust_max <= ceiling
            ok, note = ok and hit, ""
            details.append(f"fps_ceiling {ceiling}: robust_max={robust_max} "
                           f"(max={ordered[0]}): {'PASS' if hit else 'FAIL'}")
        if note:
            details.append(note.lstrip(" |"))
    # Frame-time ceilings are the real "dropped/slow frame" gates. `grade_frame_ceilings` fails
    # closed when the detector was not armed (no worstframe= samples), as upstream requires.
    ceiling_ok, ceiling_detail = harness.grade_frame_ceilings(scene, lines, route, overlay, warmup)
    ok = ok and ceiling_ok
    if ceiling_detail:
        details.append(ceiling_detail.lstrip(" |"))
    for helper in ("grade_coldopen", "grade_upload_pacing"):
        gate_ok, gate_detail = getattr(harness, helper)(scene, lines, route, overlay)
        ok = ok and gate_ok
        if gate_detail:
            details.append(gate_detail.lstrip(" |"))
    if scene.get("framedrop") is not None and not ceiling_detail:
        worst = harness.parse_worst(lines, route, overlay)
        drops = harness.parse_framedrop(lines, route)
        details.append(f"worstframe n={len(worst)}"
                       + (f", max={max(worst):.1f}ms" if worst else "")
                       + f" | FRAMEDROP lines={len(drops)}"
                       + (f", worst={max(drops):.1f}ms" if drops else ""))
    return ok, " | ".join(details)


def bench_one(args: argparse.Namespace, scene: dict, cache_label: str = "") -> str:
    """One sealed nuviolive boot, one walk, one graded frame-rate result. Never deploys.

    `cache_label` is only a report tag ("cold"/"warm"); the caller owns the cache state.
    """
    flavor = args.flavor
    if flavor != "debug":
        fail("nuvio-tv drives the debug install only (the stable install is the owner's)", 2)
    harness = fps_module()
    require_deployed_binary(flavor)

    # Timing gates must never grade the fast tvdev build. The profile is read from the set, and
    # building/deploying release stays an owner-approved step outside this tool.
    if any(scene.get(k) is not None for k in ("loop_floor", "fps_floor", "fps_ceiling",
                                             "worst_ceiling_ms", "stall_ceiling_ms",
                                             "coldopen_ceiling_ms", "upload_per_frame_min",
                                             "upload_late_share_max", "upload_frames_min")):
        appdir = make_query("print-appdir", flavor)

        def remote(command: str):
            return call([str(SSH), "ssh", "tv", command], check=False, timeout=30)

        profile, message = arm_profile_guard.check(remote, appdir, "the debug install")
        if message:
            fail(f"{message}\nTiming needs the owner's approval to build and deploy: "
                 "`make ARM_PROFILE=release deploy`", 2)
        say(f"nuvio-tv: binary profile {arm_profile_guard.describe(profile)}")

    runtime = (Path(args.runtime).resolve() if args.runtime
               else Path(tempfile.mkdtemp(prefix="nuvio-tv-bench-")))
    runtime.mkdir(parents=True, exist_ok=True)
    log = runtime / f"bench-{args.scene}.log"
    # Benchmarks use their OWN addon file and their OWN remote filename: the normal `start`/`run`
    # path must keep reading `nuvio-public-addons.json`, never the benchmark catalog.
    addon_file = (Path(scene["addons"]).resolve() if scene.get("addons")
                  else Path(args.addons).resolve() if args.addons else BENCH_ADDONS_FILE)
    if not addon_file.is_file():
        fail(f"benchmark addon list not found: {addon_file}", 2)
    rundir, app_port = make_query("print-rundir", flavor), make_query("print-appport", flavor)
    if not rundir or not app_port:
        fail("could not resolve the install's runtime root or capture port", 1)
    call([str(SSH), "ssh", "tv", f"mkdir -p {rundir}"])
    remote_addons = f"{rundir}/nuvio-benchmark-addons.json"
    call([str(SSH), "scp", str(addon_file), f"tv:{remote_addons}"])

    arm = [LIVE_TRIGGER, f"{ADDONS_TRIGGER}={remote_addons}"]
    if args.shot:
        arm.append(f"capture={app_port}")
    for extra in list(args.trigger) + list(scene.get("triggers", [])):
        tname, sep, value = extra.partition("=")
        if not tname or any(c not in "abcdefghijklmnopqrstuvwxyz0123456789_-" for c in tname):
            fail(f"bad trigger name {tname!r}", 2)
        arm.append(f"{tname}={value}" if sep else tname)
    threshold = scene.get("framedrop")
    if threshold is None:
        threshold = harness.frame_ceiling_threshold(scene)
    if threshold is not None:
        # `tv-session.sh up` adds the `plxnative-` prefix to every --arm name.
        arm.append(f"framedrop={threshold}")
    up = [str(SESSION), "up", "--flavor", flavor, "--screen", scene.get("screen", "home"),
          "--no-token"]
    for trigger in arm:
        up += ["--arm", trigger]

    # Boot proofs are watched before the walk; scene/CLI expectations are checked against the FINAL
    # log, because many of them only appear after a key or a press.
    expected = list(scene.get("expect", [])) + list(args.expect)
    try:
        result = call(up, check=False, timeout=max(args.timeout, 180))
        if result.returncode:
            fail(f"TV session did not come up:\n{result.stdout}{result.stderr}".rstrip(), 1)
        say(result.stdout.strip())
        # Owner's standing rule: muted for every device run, benchmarks included. The panel is
        # blanked too unless --panel-on: the owner may leave it lit to watch an interactive bench.
        verbs = ("sound",) if args.panel_on else ("sound", "screen")
        for verb in verbs:
            quiet = call([str(SESSION), verb, "off", "--flavor", flavor], check=False)
            if quiet.returncode:
                warn_control(verb, quiet)
        _, seen = watch_tv_log(list(LIVE_PROOFS), args.timeout, flavor)
        say(f"nuvio-tv: live boot proved ({len(seen)} lines)")
        if scene.get("open_detail"):
            _, rk = pick_detail(flavor, runtime)
            watch_tv_log([f"rk={rk} show="], args.timeout, flavor)
            expected.append(f"rk={rk} show=")
        time.sleep(float(scene.get("settle_s", args.settle)))
        # The scrolling verdict starts at the first key, not at boot: a first-mount stall is a
        # separate boot cost and must not be confused with a hitch while browsing.
        walk_start = len(tv_log(flavor).splitlines())
        gap = float(scene.get("key_gap", args.key_gap))
        pause_after, pause_s = scene.get("pause_after"), float(scene.get("pause_s", 2))
        tokens = sanitize(" ".join(scene.get("keys", [])))
        cut = int(pause_after) if pause_after else None
        legs = [tokens[:cut], tokens[cut:]] if cut else [tokens]
        cold_cut = 0
        for index, leg in enumerate(legs):
            if not leg:
                continue
            # One ssh call for the whole leg. `key` spends ~2s per token in fifo_send's time box,
            # which stretched a 0.5s cadence to ~2.5s; `keyseq` spaces the writes on the set.
            result = call([str(SESSION), "keyseq", f"{gap:.1f}", *leg, "--flavor", flavor],
                          check=False, timeout=max(args.timeout, 180))
            if result.returncode:
                fail(f"TV keyseq {leg[0]!r} refused:\n{result.stdout}{result.stderr}".rstrip(), 1)
            say(f"  keys: {' '.join(leg)} @ {gap:.1f}s")
            if index + 1 < len(legs):
                # Split the log at the rest so down/up leg counts can be reported separately.
                cold_cut = len(tv_log(flavor).splitlines())
                say(f"  pause {pause_s:g}s (down → up)")
                time.sleep(pause_s)
        time.sleep(float(scene.get("run_s", 8)))
        final = tv_log(flavor)
        log.write_text(final, encoding="utf-8")
        missing = [n for n in expected if n not in final]
        if missing:
            fail(f"bench {args.scene}: final TV log lost expected lines {missing} (log: {log})", 1)
        ok, detail = grade_scene(scene, final.splitlines(), harness)
        pieces = final.splitlines()
        route, overlay = scene["route"], scene.get("overlay")
        # The smoothness verdict is about BROWSING: grade only from the first key onward. A first
        # mount stall is real, but it is a boot cost and is reported separately, not held against
        # the scroll. `walk_drop_max` and `walk_max_ms` are the fork's explicit "no jank" contract.
        drop_limit, frame_limit = scene.get("walk_drop_max"), scene.get("walk_max_ms")
        if drop_limit is not None or frame_limit is not None:
            threshold = float(scene.get("framedrop", 22))
            walk_drops = harness.parse_framedrop(pieces[walk_start:], route)
            over = sum(v > threshold for v in walk_drops)
            peak = max(walk_drops) if walk_drops else 0.0
            boot_peak = max(harness.parse_framedrop(pieces[:walk_start], route), default=0.0)
            passed = ((drop_limit is None or over <= drop_limit)
                      and (frame_limit is None or peak <= frame_limit))
            ok = ok and passed
            parts = [f"walk: {over} frame(s) >{threshold:g}ms",
                     f"max={peak:.1f}ms",
                     f"limit: drops<={drop_limit}, max<={frame_limit}ms",
                     "PASS" if passed else "FAIL",
                     f"boot max={boot_peak:.1f}ms (informational)"]
            detail += " | " + ", ".join(parts)
        if cold_cut > walk_start:
            legs_detail = []
            for label, part in (("down", pieces[walk_start:cold_cut]), ("up", pieces[cold_cut:])):
                worst = harness.parse_worst(part, route, overlay)
                drops = harness.parse_framedrop(part, route)
                peak = f", peak={max(drops):.1f}ms" if drops else ""
                legs_detail.append(f"{label}: worst={max(worst):.1f}ms, drops={len(drops)}{peak}"
                                   if worst else f"{label}: no worstframe samples")
            detail += " | " + ", ".join(legs_detail)
        if args.shot:
            capture_shot(flavor, rundir, runtime, args.shot)
        tag = f" [{cache_label}]" if cache_label else ""
        if not ok:
            fail(f"bench {args.scene}{tag} FAIL — {detail}\nlog: {log}", 1)
        say(f"nuvio-tv: bench {args.scene}{tag} PASS — {detail}\nlog: {log}")
        return detail
    finally:
        down = call([str(SESSION), "down", "--flavor", flavor], check=False)
        if down.returncode:
            say("nuvio-tv: WARNING — `down` did not complete; hand the TV back yourself")


def bench(args: argparse.Namespace) -> None:
    """One scene, or `--suite`: every declared scene in turn, each in its own sealed boot."""
    scenes = load_scenes()
    if not args.suite:
        bench_one(args, scenes[args.scene])
        return
    names = [name for name in scenes if not args.scene or args.scene in name]
    if not names:
        fail(f"no bench scene matches {args.scene!r}", 2)
    results: list[tuple[str, int]] = []
    for name in names:
        one = argparse.Namespace(**vars(args))
        one.scene = name
        one.shot = f"suite-{name}" if args.shot else None
        try:
            bench_one(one, scenes[name])
        except SystemExit as exc:
            results.append((name, exc.code or 1))
        else:
            results.append((name, 0))
    say("nuvio-tv: suite " + ", ".join(f"{n}={'PASS' if rc == 0 else 'FAIL'}" for n, rc in results))
    if any(rc for _, rc in results):
        raise SystemExit(1)


def gate(args: argparse.Namespace) -> None:
    """Local PRE-RELEASE gate: a PURE cold-cache boot, then warm-cache boots that reuse it.

    Run 1 deletes the debug install's image cache and nuvio.redb before booting, so it measures a
    cold catalog/art cache and a brand-new store. Runs 2..N reuse exactly what run 1 populated.
    Each boot is graded; the saved evidence carries both the cold and warm numbers so the difference
    is visible. This is deliberately not in CI (no TV there) and never touches the stable install.
    """
    flavor = args.flavor
    scenes = load_scenes()
    scene = scenes.get(args.scene)
    if scene is None:
        fail(f"unknown gate scene {args.scene!r}; see `nuvio-tv.py scenes`", 2)
    runs = int(args.runs)
    if runs < 1:
        fail("--runs must be at least 1", 2)
    require_deployed_binary(flavor)
    runtime = (Path(args.runtime).resolve() if args.runtime
               else Path(tempfile.mkdtemp(prefix="nuvio-tv-gate-")))
    runtime.mkdir(parents=True, exist_ok=True)
    detail: list[dict] = []
    for i in range(1, runs + 1):
        cache_label = "cold" if i == 1 else "warm"
        if i == 1:
            say(f"nuvio-tv: cold cache reset — {cold_cache_reset(flavor)}")
        one = argparse.Namespace(**vars(args))
        one.shot = f"gate-{args.scene}-{i}" if args.shot else None
        try:
            grade = bench_one(one, scene, cache_label)
        except SystemExit as exc:
            rc, note = exc.code or 1, "failed (see the run log)"
        else:
            rc, note = 0, grade
        detail.append({"run": i, "cache": cache_label, "result": rc, "grade": note})
    passed = sum(item["result"] == 0 for item in detail)
    verdict = passed > runs // 2
    evidence = runtime / f"gate-{args.scene}.json"
    evidence.write_text(json.dumps({"scene": args.scene, "runs": runs, "detail": detail,
                                    "passed": passed, "verdict": "PASS" if verdict else "FAIL"},
                                   indent=2), encoding="utf-8")
    say(f"nuvio-tv: gate {args.scene} {passed}/{runs} PASS — {evidence}")
    if not verdict:
        raise SystemExit(1)


def selftest() -> None:
    """Offline wiring check: no ssh, no lock, no television."""
    if not ADDONS_FILE.is_file():
        fail(f"missing tracked addon list: {ADDONS_FILE}")
    urls = json.loads(ADDONS_FILE.read_text(encoding="utf-8")).get("addons")
    if not isinstance(urls, list) or not urls or not all(
            isinstance(u, str) and u.startswith("https://") for u in urls):
        fail("the addon list must be a non-empty array of https URLs")
    if not BENCH_ADDONS_FILE.is_file():
        fail(f"missing benchmark-only addon list: {BENCH_ADDONS_FILE}")
    bench_urls = json.loads(BENCH_ADDONS_FILE.read_text(encoding="utf-8")).get("addons")
    if not isinstance(bench_urls, list) or not bench_urls or not all(
            isinstance(u, str) and u.startswith("https://") for u in bench_urls):
        fail("the benchmark addon list must be a non-empty array of https URLs")
    source = DEBUG_MODULE.read_text(encoding="utf-8")
    if "nuvio-benchmark-addons.json" not in Path(__file__).read_text(encoding="utf-8"):
        fail("benchmark boots must stage a distinct remote addon filename")
    for constant in (f'"{LIVE_TRIGGER}"', f'"{ADDONS_TRIGGER}"'):
        if constant not in source:
            fail(f"app module no longer declares {constant}: trigger wiring drifted")
    for needle in LIVE_PROOFS:
        if needle not in (REPO / "tools/nuvio-sim.py").read_text(encoding="utf-8"):
            fail(f"live proof drifted from the host loop: {needle}")
    for tool, flag in ((SESSION, "--arm"), (SESSION, "--no-token"), (SESSION, "setpriv"),
                       (SESSION, "turnOffScreen"), (SESSION, "setMuted"),
                       (LOCK, "with"), (SSH, "scp"),
                       (REPO / "tools/stream-screen.py", "--codec")):
        if not tool.is_file() or flag not in tool.read_text(encoding="utf-8", errors="replace"):
            fail(f"{tool.name} no longer supports {flag}: TV wiring drifted")
    makefile = MAKEFILE.read_text(encoding="utf-8", errors="replace")
    if "TMDB_API_KEY" not in makefile or "nuvio.local.env" not in makefile:
        fail("the ARM build no longer bakes in the dedicated TMDB key")
    scenes = load_scenes()
    harness = fps_module()
    for helper in ("parse_loop", "parse_fps", "rate_stats", "fps_floor_verdict",
                   "parse_framedrop", "parse_worst", "grade_frame_ceilings",
                   "frame_ceiling_threshold", "grade_coldopen", "grade_upload_pacing"):
        if not hasattr(harness, helper):
            fail(f"tests/run.py no longer exposes {helper}: bench would invent its own gates")
    source = Path(__file__).read_text(encoding="utf-8")
    if source.count("require_deployed_binary(flavor)") < 3:
        fail("every boot path must refuse a stale binary rather than deploy it")
    if "arm_profile_guard.check" not in source:
        fail("timing scenes no longer gate on the deployed binary's profile")
    assert sanitize("down, right ok") == ["down", "right", "ok"]
    assert normalize_argv(["bench", "--scene", "home-nav"]) == ["--scene", "home-nav", "bench"]
    assert normalize_argv(["press", "card.0", "--rk", "7"]) == ["--rk", "7", "press", "card.0"]
    assert normalize_argv(["key", "down", "down", "ok"]) == ["key", "down", "down", "ok"]
    assert normalize_argv(["bench", "--suite", "--scene", "home"]) == [
        "--suite", "--scene", "home", "bench"]
    assert normalize_argv(["gate", "--scene", "home-nav-deep", "--runs", "3"]) == [
        "--scene", "home-nav-deep", "--runs", "3", "gate"]
    assert normalize_argv(["bench", "--scene", "home-nav-deep", "--panel-on"]) == [
        "--scene", "home-nav-deep", "--panel-on", "bench"]
    # Fail-closed lease check, fully offline: a refused `require` must stop the verb before any
    # tv-session command. Mock `call` to behave like the real one (raise on check=True failure).
    real_call, real_session = call, SESSION_FILE
    try:
        with tempfile.TemporaryDirectory(prefix="nuvio-tv-selftest-") as tmp:
            globals()["SESSION_FILE"] = Path(tmp) / "session.json"
            SESSION_FILE.write_text(json.dumps({"flavor": "debug", "rundir": "/tmp/fake",
                                                "app_port": 8911, "runtime": tmp,
                                                "screen": "home"}), encoding="utf-8")
            seen: list[list[str]] = []

            def fake_call(cmd: list[str], **kwargs):
                seen.append(cmd)
                refused = "require" in cmd
                if kwargs.get("check", True) and refused:
                    raise SystemExit(1)
                return subprocess.CompletedProcess(cmd, 1 if refused else 0, "", "")

            globals()["call"] = fake_call
            try:
                session_action("shot", argparse.Namespace(rest=[], timeout=1, rk=None))
            except SystemExit as exc:
                assert exc.code == 1
            else:
                fail("session verb drove the TV after a refused lease check")
            assert not any("tv-session.sh" in cmd for cmd in seen), "lease refusal was not fail-closed"
    finally:
        globals()["call"] = real_call
        globals()["SESSION_FILE"] = real_session
    say(f"nuvio-tv: selftest OK (offline, {len(urls)} public addons, "
        f"{len(scenes)} bench scenes)")


def parser() -> argparse.ArgumentParser:
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("mode", nargs="?", default="run",
                   choices=["run", "selftest", "start", "shot", "key", "state", "press",
                            "detail", "log", "expect", "status", "stop", "bench", "gate", "scenes"],
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
    p.add_argument("--scene", metavar="NAME",
                   help="bench scene from tests/nuvio-tv-scenes.json (`nuvio-tv.py scenes` lists them)")
    p.add_argument("--suite", action="store_true",
                   help="with bench: run every declared scene in turn, each in its own sealed boot")
    p.add_argument("--runs", type=int, default=2,
                   help="with gate: boots; run 1 cold (cache erased), the rest warm (reuse) "
                        "(default 2); majority decides PASS")
    p.add_argument("--panel-on", action="store_true",
                   help="with bench/gate: leave the TV panel lit instead of blanking it")
    return p


if __name__ == "__main__":
    ns = parser().parse_args(normalize_argv(sys.argv[1:]))
    if ns.mode == "selftest":
        selftest()
    elif ns.mode == "scenes":
        for name, scene in load_scenes().items():
            say(f"{name:16} route={scene['route']:8} screen={scene.get('screen', 'home')} "
                f"— {scene.get('note', '')}")
    elif ns.mode == "start":
        session_start(ns)
    elif ns.mode == "stop":
        session_stop()
    elif ns.mode in ("shot", "key", "state", "press", "detail", "log", "expect", "status"):
        session_action(ns.mode, ns)
    elif ns.mode in ("bench", "gate") and os.environ.get("NUVIOTV_INNER") != "1":
        # Usage errors must fail before `tv-lock.sh with` takes a lease or wakes the set.
        if ns.flavor != "debug":
            fail("nuvio-tv drives the debug install only (the stable install is the owner's)", 2)
        scenes = load_scenes()
        if ns.mode == "gate":
            if not ns.scene or ns.scene not in scenes:
                fail("gate needs --scene NAME from `nuvio-tv.py scenes`", 2)
        elif ns.suite:
            if ns.scene and not any(ns.scene in name for name in scenes):
                fail(f"no bench scene matches {ns.scene!r}", 2)
        elif not ns.scene or ns.scene not in scenes:
            fail("bench needs --scene NAME from `nuvio-tv.py scenes`", 2)
        # Good arguments: fall through to the same lease-taking wrapper as the one-shot form.
        env = os.environ.copy()
        env["NUVIOTV_INNER"] = "1"
        raise SystemExit(subprocess.run(
            [str(LOCK), "with", "--ttl", "30", "--why", f"nuvio-tv {ns.mode}", "--",
             sys.executable, str(Path(__file__).resolve()), *sys.argv[1:]],
            cwd=REPO, env=env).returncode)
    elif os.environ.get("NUVIOTV_INNER") != "1":
        # Re-enter inside `with` so the lease is released on Ctrl-C, SIGTERM and a crash too.
        env = os.environ.copy()
        env["NUVIOTV_INNER"] = "1"
        raise SystemExit(subprocess.run(
            [str(LOCK), "with", "--ttl", "30", "--why", "nuvio-tv", "--",
             sys.executable, str(Path(__file__).resolve()), *sys.argv[1:]],
            cwd=REPO, env=env).returncode)
    elif ns.mode == "bench":
        bench(ns)
    elif ns.mode == "gate":
        gate(ns)
    else:
        run(ns)

#!/usr/bin/env python3
"""NuvioNative device helper for a Developer Mode (non-rooted) LG TV, driven through the webOS CLI.

    tools/nv-tv.py install <ipk>
    tools/nv-tv.py launch|close [--appid ID]
    tools/nv-tv.py log [N]                      # tail the app's event log
    tools/nv-tv.py keys down down ok back       # press keys through the app's remote FIFO
    tools/nv-tv.py trigger NAME [VALUE]         # write /tmp/<appid>/plxnative-NAME (debug builds)
    tools/nv-tv.py untrigger NAME
    tools/nv-tv.py shot out.jpg [--w 960 --h 540]  # UI-plane screenshot via the in-app capture
                                                  # listener (needs the `capture` trigger at launch)

The device is the ares profile named by $NV_DEVICE (default `tv`); its address is read from
`ares-setup-device --list`. Nothing here needs root.
"""
import argparse
import json
import re
import shlex
import socket
import struct
import subprocess
import sys
import time

DEVICE = __import__("os").environ.get("NV_DEVICE", "tv")
DEFAULT_APPID = "com.nuvionative.app.debug"


def redact(text):
    """Debrid credentials can occur anywhere in a URL, including its path."""
    return re.sub(r"https?://[^\s]+", "<url>", text, flags=re.IGNORECASE)


def run(cmd, check=True, capture=True):
    r = subprocess.run(cmd, capture_output=capture, text=True)
    if check and r.returncode != 0:
        sys.exit(redact(f"{' '.join(cmd)} failed:\n{r.stdout}\n{r.stderr}"))
    return redact(r.stdout)


def sh(remote, check=False):
    out = run(["ares-novacom", "--device", DEVICE, "--run", remote], check=check)
    return "\n".join(l for l in out.splitlines() if not l.startswith("[Info]"))


def tv_host():
    out = run(["ares-setup-device", "--list"])
    for line in out.splitlines():
        if line.split() and line.split()[0] == DEVICE:
            m = re.search(r"@([0-9.]+):", line)
            if m:
                return m.group(1)
    sys.exit(f"device {DEVICE} not found in ares-setup-device --list")


def rundir(appid):
    return "/tmp" if appid.count(".") == 2 else f"/tmp/{appid}"


def shot(path, appid, w, h, port, frames=1):
    host = tv_host()
    s = socket.create_connection((host, port), timeout=10)
    s.sendall(b"PXRQ" + struct.pack("<HH", w, h))

    def exact(n):
        b = bytearray()
        while len(b) < n:
            c = s.recv(n - len(b))
            if not c:
                sys.exit("capture: connection closed")
            b.extend(c)
        return bytes(b)

    # A connection may initially receive the last captured frame. While changing the UI,
    # --frames 3 lets the GPU capture chain advance before saving the screenshot.
    data = b""
    for _ in range(frames):
        hdr = exact(16)
        if hdr[:4] != b"PXFR":
            sys.exit(f"capture: bad magic {hdr[:4]!r}")
        n = struct.unpack("<I", hdr[4:8])[0]
        data = exact(n)
    s.close()
    open(path, "wb").write(data)
    print(f"wrote {path} ({n} bytes)")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--appid", default=DEFAULT_APPID)
    sub = ap.add_subparsers(dest="cmd", required=True)
    p = sub.add_parser("install"); p.add_argument("ipk")
    sub.add_parser("launch"); sub.add_parser("close")
    p = sub.add_parser("log"); p.add_argument("n", nargs="?", default="60")
    p = sub.add_parser("grep"); p.add_argument("pattern")
    p = sub.add_parser("keys"); p.add_argument("keys", nargs="+"); p.add_argument("--gap", type=float, default=0.35)
    p = sub.add_parser("trigger"); p.add_argument("name"); p.add_argument("value", nargs="?")
    p = sub.add_parser("untrigger"); p.add_argument("name")
    p = sub.add_parser("shot"); p.add_argument("out"); p.add_argument("--w", type=int, default=960); p.add_argument("--h", type=int, default=540); p.add_argument("--port", type=int, default=None); p.add_argument("--frames", type=int, default=1)
    p = sub.add_parser("sh"); p.add_argument("command")
    a = ap.parse_args()
    rd = rundir(a.appid)
    if a.cmd == "install":
        print(run(["ares-install", "--device", DEVICE, a.ipk]))
    elif a.cmd == "launch":
        print(run(["ares-launch", "--device", DEVICE, a.appid]))
    elif a.cmd == "close":
        print(run(["ares-launch", "--device", DEVICE, "--close", a.appid], check=False))
    elif a.cmd == "log":
        print(sh(f"tail -n {int(a.n)} {rd}/plxnative-events.log"))
    elif a.cmd == "grep":
        print(sh(f"grep -a -E {shlex.quote(a.pattern)} {rd}/plxnative-events.log | tail -n 80"))
    elif a.cmd == "keys":
        # Space checked writes on the host; this TV’s BusyBox sleep accepts only integers.
        for key in a.keys:
            print(sh(f"echo {shlex.quote(key)} > {rd}/plxnative-remote", check=True))
            time.sleep(max(0, a.gap))
    elif a.cmd == "trigger":
        val = a.value if a.value is not None else ""
        print(sh(f"mkdir -p {rd} && printf %s {shlex.quote(val)} > {rd}/plxnative-{a.name} && ls -la {rd}/plxnative-{a.name}"))
    elif a.cmd == "untrigger":
        print(sh(f"rm -f {rd}/plxnative-{a.name}"))
    elif a.cmd == "shot":
        port = a.port or (8910 if a.appid.count(".") == 2 else 8911)
        shot(a.out, a.appid, a.w, a.h, port, max(1, a.frames))
    elif a.cmd == "sh":
        print(sh(a.command))


if __name__ == "__main__":
    main()

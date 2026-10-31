#!/usr/bin/env python3
"""Offline Stremio-addon / TMDB prober.

Usage:
  python tools/probe-addon.py manifest <url>
  python tools/probe-addon.py meta <url> <type> <id>
  python tools/probe-addon.py streams <url> <type> <id>
  python tools/probe-addon.py tmdb <type> <id>

No app, no TV, no ARM toolchain required.
"""
import json
import os
import sys
import urllib.parse
import urllib.request

UA = "nuvio-probe/1.0"


def canonicalize_base(url: str) -> str:
    s = url.strip()
    if s.lower().startswith("stremio://"):
        s = "https://" + s[len("stremio://"):]
    # split query
    if "?" in s:
        path, query = s.split("?", 1)
        query = "?" + query
    else:
        path, query = s, ""
    path = path.rstrip("/")
    low = path.lower()
    if low.endswith("/manifest.json"):
        path = path[: -len("/manifest.json")].rstrip("/")
    return path + query


def fetch_json(url: str):
    req = urllib.request.Request(url, headers={"User-Agent": UA, "Accept": "application/json"})
    with urllib.request.urlopen(req, timeout=20) as resp:
        raw = resp.read().decode("utf-8", "replace")
    return json.loads(raw)


def enc(s: str) -> str:
    return urllib.parse.quote(str(s), safe="")


def cmd_manifest(url: str) -> int:
    base = canonicalize_base(url)
    murl = base + "/manifest.json" if not base.lower().endswith("manifest.json") else base
    # base already stripped manifest; ensure single
    if not murl.lower().endswith("/manifest.json"):
        murl = base + "/manifest.json"
    data = fetch_json(murl)
    print("=== MANIFEST ===")
    print("id:   ", data.get("id"))
    print("name: ", data.get("name"))
    print("version:", data.get("version"))
    print("description:", data.get("description"))
    print("resources:", data.get("resources"))
    print("types:   ", data.get("types"))
    print("--- catalogs ---")
    for c in data.get("catalogs", []) or []:
        print(f"  {c.get('type')}/{c.get('id')}  name={c.get('name')!r}  extra={c.get('extra')}")
    print("--- behaviorHints ---")
    print(json.dumps(data.get("behaviorHints", {}), indent=2))
    return 0


def resource_url(base: str, resource: str, type_: str, id_: str) -> str:
    b = canonicalize_base(base)
    # strip any trailing manifest.json already handled in canonicalize
    return f"{b}/{resource}/{enc(type_)}/{enc(id_)}.json"


def cmd_meta(url: str, type_: str, id_: str) -> int:
    u = resource_url(url, "meta", type_, id_)
    print("URL:", u)
    data = fetch_json(u)
    meta = data.get("meta") or data
    print("=== META ===")
    print("id:      ", meta.get("id"))
    print("type:    ", meta.get("type"))
    print("name:    ", meta.get("name"))
    print("title:   ", meta.get("title") or meta.get("name"))
    print("year:    ", meta.get("year") or meta.get("releaseInfo"))
    print("released:", meta.get("released"))
    print("runtime: ", meta.get("runtime"))
    print("genres:  ", meta.get("genres"))
    print("imdb:    ", meta.get("imdb_id") or meta.get("imdbId"))
    print("poster:  ", meta.get("poster"))
    print("background:", meta.get("background"))
    desc = meta.get("description") or meta.get("plot")
    if desc:
        print("description:", (desc[:400] + ("..." if len(desc) > 400 else "")))
    cast = meta.get("cast") or []
    if cast:
        print("cast:", ", ".join(str(c) for c in cast[:20]))
    videos = meta.get("videos") or []
    if videos:
        print("videos/episodes:", len(videos))
    return 0


def fmt_size(n):
    try:
        n = int(n)
    except (TypeError, ValueError):
        return str(n)
    for unit in ("B", "KB", "MB", "GB", "TB"):
        if n < 1024:
            return f"{n:.1f} {unit}" if unit != "B" else f"{n} B"
        n /= 1024
    return f"{n:.1f} PB"


def cmd_streams(url: str, type_: str, id_: str) -> int:
    u = resource_url(url, "stream", type_, id_)
    print("URL:", u)
    data = fetch_json(u)
    streams = data.get("streams") or []
    print(f"=== STREAMS ({len(streams)}) ===")
    for i, s in enumerate(streams, 1):
        name = s.get("name") or s.get("title") or "(unnamed)"
        title = s.get("title") or ""
        quality = s.get("quality") or ""
        size = s.get("size") or s.get("bytes")
        urlv = s.get("url")
        infohash = s.get("infoHash")
        print(f"[{i}] {name}")
        if title and title != name:
            print(f"     title: {title}")
        if quality:
            print(f"     quality: {quality}")
        if size is not None:
            print(f"     size: {fmt_size(size)}")
        if urlv:
            print(f"     url: {urlv[:120]}{'...' if len(urlv) > 120 else ''}")
        if infohash:
            print(f"     infoHash: {infohash}")
            subs = s.get("fileIdx")
            if subs is not None:
                print(f"     fileIdx: {subs}")
    return 0


def load_env():
    # .env in cwd or repo root
    for cand in (".env", os.path.join(os.path.dirname(__file__), "..", ".env")):
        p = os.path.abspath(cand)
        if os.path.isfile(p):
            with open(p) as f:
                for line in f:
                    line = line.strip()
                    if not line or line.startswith("#") or "=" not in line:
                        continue
                    k, v = line.split("=", 1)
                    k = k.strip()
                    v = v.strip().strip('"').strip("'")
                    if k and k not in os.environ:
                        os.environ[k] = v
    # also allow direct env
    return os.environ.get("TMDB_API_KEY", "").strip()


def cmd_tmdb(type_: str, id_: str) -> int:
    api_key = load_env()
    if not api_key:
        print("TMDB_API_KEY not found in .env or environment.", file=sys.stderr)
        return 2
    t = type_.lower()
    if t in ("series", "tv", "show"):
        t = "tv"
    elif t in ("movie", "film"):
        t = "movie"
    path = f"/{t}/{id_}?append_to_response=credits"
    url = f"https://api.themoviedb.org/3{path}&api_key={urllib.parse.quote(api_key, safe='')}"
    print("URL:", url.replace(api_key, "***"))
    data = fetch_json(url)
    print("=== TMDB ===")
    title = data.get("title") or data.get("name")
    print("title:   ", title)
    print("original:", data.get("original_title") or data.get("original_name"))
    print("year:    ", (data.get("release_date") or data.get("first_air_date") or "")[:4])
    print("genres:  ", [g.get("name") for g in (data.get("genres") or [])])
    print("rating:  ", data.get("vote_average"))
    print("runtime: ", data.get("runtime") or data.get("episode_run_time"))
    overview = data.get("overview")
    if overview:
        print("overview:", overview)
    credits = data.get("credits") or {}
    cast = credits.get("cast") or []
    if cast:
        print("cast:", ", ".join(c.get("name", "") for c in cast[:15]))
    # collection
    belongs = data.get("belongs_to_collection")
    if belongs:
        print("collection:", belongs.get("name"))
    return 0


def main(argv):
    if len(argv) < 2:
        print(__doc__)
        return 1
    mode = argv[1].lower()
    try:
        if mode == "manifest":
            if len(argv) < 3:
                print("usage: probe-addon.py manifest <url>", file=sys.stderr)
                return 1
            return cmd_manifest(argv[2])
        if mode == "meta":
            if len(argv) < 5:
                print("usage: probe-addon.py meta <url> <type> <id>", file=sys.stderr)
                return 1
            return cmd_meta(argv[2], argv[3], argv[4])
        if mode == "streams":
            if len(argv) < 5:
                print("usage: probe-addon.py streams <url> <type> <id>", file=sys.stderr)
                return 1
            return cmd_streams(argv[2], argv[3], argv[4])
        if mode == "tmdb":
            if len(argv) < 4:
                print("usage: probe-addon.py tmdb <type> <id>", file=sys.stderr)
                return 1
            return cmd_tmdb(argv[2], argv[3])
        print(f"unknown mode: {mode}", file=sys.stderr)
        return 1
    except Exception as e:
        print(f"ERROR: {e}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main(sys.argv))
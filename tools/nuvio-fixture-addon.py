#!/usr/bin/env python3
"""A hermetic local Stremio addon for the nuviolive loop.

Two shapes, one mechanism:

    # Legacy single-title fixture (docs/nuvio/SCENARIO.md S12-S15):
    python3 tools/nuvio-fixture-addon.py PORT tt33764258 /path/to/local.mp4

    # Full deterministic catalog mock (the fork's answer to upstream's mock_pms.py):
    python3 tools/nuvio-fixture-addon.py --port PORT --movies 300 --shows 12 \
        --home-hubs 41 --media /path/to/local.mp4 --link 3g

The full shape serves real Stremio resources on loopback only: `/manifest.json`,
paged `/catalog/...`, `/meta/...`, `/stream/...` (with a bounded, labelled source list),
`/subtitles/...`, locally generated artwork with realistic decoded size and long tokenized
URLs, and Range-capable bytes for one local MP4. No torrent, debrid key, account, CDN or TV
is involved. It exists so routine coverage stops depending on rate-limited public addons.

Artwork and metadata are generated from `--seed`, so two runs with one seed show the same
catalog, the same source list and the same subtitle tracks. `--link` reuses the exact
`tests/link_conditioner.py` profiles upstream grades its paging scenes with; both link
classes are set to the same profile because Stremio paths have no PMS-style class split.

    python3 tools/nuvio-fixture-addon.py --port 18765 --seed 11 --link remote-wan
    # then put {"addons": ["http://127.0.0.1:18765"]} in a private --addons list.
"""

import argparse
import hashlib
import http.server
import json
import os
import re
import struct
import sys
import time
import urllib.parse
import zlib

sys.path.insert(0, str(os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "tests")))
import link_conditioner  # noqa: E402 - local module, resolved next to this tool

ADJECTIVES = ("Crimson", "Silent", "Broken", "Golden", "Hollow", "Distant", "Frozen", "Last",
              "Burning", "Quiet", "Hidden", "Bitter", "Pale", "Restless", "Savage", "Fatal")
NOUNS = ("Harbor", "Meridian", "Lantern", "Circuit", "Orchard", "Signal", "Requiem", "Frontier",
         "Archive", "Cascade", "Parallax", "Threshold", "Empire", "Voyage", "Kindred", "Aftermath")
LANGS = ("eng", "alb", "ind", "spa", "fra", "deu", "por", "ita", "pol", "tur", "ara", "kor")


def digest(*parts: object) -> str:
    return hashlib.sha256("|".join(str(p) for p in parts).encode()).hexdigest()


def title(seed: int, kind: str, index: int) -> str:
    a = int(digest(seed, kind, index, "a")[:8], 16) % len(ADJECTIVES)
    b = int(digest(seed, kind, index, "b")[:8], 16) % len(NOUNS)
    return f"{ADJECTIVES[a]} {NOUNS[b]}"


def png_chunk(tag: bytes, data: bytes) -> bytes:
    return struct.pack(">I", len(data)) + tag + data + struct.pack(">I", zlib.crc32(tag + data))


_ART_CACHE = {}


def art_png(seed: int, item_id: str, width: int, height: int) -> bytes:
    """Deterministic, moderately noisy RGB art with a realistic decoded size.

    A catalog of 300 items shares a small bucket set: generating a fresh noise field per pixel
    per poster used to stall the fixture server (and starve the simulator's fps) on first load.
    The formula is cheap and the cache is global, so 300 posters cost 16 images, not 300.
    """
    bucket = int(digest(seed, item_id, "art")[:4], 16) % 16
    key = (seed, bucket, width, height)
    if key in _ART_CACHE:
        return _ART_CACHE[key]
    rows = []
    for y in range(height):
        row = bytearray(b"\x00")
        for x in range(width):
            mix = (x * 3 + y * 5 + bucket * 17) & 0xFF
            row += bytes((mix, ((x * 7 + y * 11 + bucket * 29) ^ mix) & 0xFF,
                          ((x * y + bucket) ^ (y * 3)) & 0xFF))
        rows.append(bytes(row))
    ihdr = struct.pack(">IIBBBBB", width, height, 8, 2, 0, 0, 0)
    body = (b"\x89PNG\r\n\x1a\n" + png_chunk(b"IHDR", ihdr)
            + png_chunk(b"IDAT", zlib.compress(b"".join(rows), 6)) + png_chunk(b"IEND", b""))
    _ART_CACHE[key] = body
    return body


def srt(track: str) -> bytes:
    return (f"1\n00:00:01,000 --> 00:00:04,000\n[{track}] local fixture subtitle\n\n"
            f"2\n00:00:05,000 --> 00:00:09,000\nNuvio Native deterministic {track}\n").encode()


class Fixture:
    def __init__(self, args):
        self.args = args
        self.seed = args.seed
        self.featured = args.featured
        self.movies = [{"type": "movie", "id": args.featured, "name": "The Odyssey (fixture)"}]
        self.movies += [{"type": "movie", "id": f"tt{7000000 + i:07d}",
                         "name": title(args.seed, "movie", i)} for i in range(args.movies)]
        self.shows = [{"type": "series", "id": f"tt{8000000 + i:07d}",
                       "name": title(args.seed, "series", i)} for i in range(args.shows)]
        self.catalogs = []
        movie_hubs = max(0, min(args.home_hubs, args.movies))
        for i in range(movie_hubs):
            chunk = self.movies[i::movie_hubs]
            self.catalogs.append(("movie", f"nuvio.movies.{i}", f"Movies {i + 1}", chunk))
        show_hubs = max(0, min(args.section_hubs, args.shows))
        for i in range(show_hubs):
            chunk = self.shows[i::show_hubs]
            self.catalogs.append(("series", f"nuvio.shows.{i}", f"Shows {i + 1}", chunk))
        if args.cw_art_fixture:
            self.catalogs.append(("movie", "nuvio.continue", "Continue Watching",
                                  self.movies[:min(20, len(self.movies))]))
        if args.lead_series and self.shows:
            # Fork Rofocale11: by-id scenes need a known show at a fixed position on Home,
            # without guessing a synthetic ratingKey that is rebuilt on every boot.
            series_shelf = [("series", "nuvio.lead", "Featured Series", self.shows[:1])]
            self.catalogs = series_shelf + self.catalogs
        self._art = {}

    def base(self, port: int) -> str:
        return f"http://127.0.0.1:{port}"

    def token(self, item_id: str) -> str:
        raw = digest(self.seed, item_id, "token")
        return (raw * (self.args.token_len // len(raw) + 1))[:self.args.token_len]

    def art_url(self, port: int, item_id: str, kind: str) -> str:
        return (f"{self.base(port)}/art/{kind}/{item_id}.png"
                f"?token={self.token(item_id)}")

    def art(self, item_id: str, kind: str) -> bytes:
        key = (item_id, kind)
        if key not in self._art:
            w = self.args.art_w if kind == "poster" else self.args.art_w * 16 // 9
            h = self.args.art_h if kind == "poster" else self.args.art_h * 9 // 16
            self._art[key] = art_png(self.seed, item_id, int(w), int(h))
        return self._art[key]

    def item_by_id(self, item_id: str):
        for item in self.movies + self.shows:
            if item["id"] == item_id:
                return item
        return None

    def meta(self, port: int, item: dict) -> dict:
        item_id = item["id"]
        meta = {
            "id": item_id, "type": item["type"], "name": item["name"],
            "poster": self.art_url(port, item_id, "poster"),
            "background": self.art_url(port, item_id, "background"),
            "description": f"Deterministic local fixture for {item_id}.",
            "releaseInfo": "2024", "released": "2024-01-01T00:00:00.000Z",
            "runtime": "108 min", "genres": ["Adventure", "Drama"],
            "cast": ["Fixture Actor A", "Fixture Actor B", "Fixture Actor C"],
            "director": ["Fixture Director"], "imdbRating": "7.4",
            "moviedb_id": int(digest(self.seed, item_id, "tmdb")[:8], 16) % 900000,
            "trailers": [], "videos": [],
        }
        if item["type"] == "series":
            for season in range(1, self.args.seasons + 1):
                for ep in range(1, self.args.episodes_per_season + 1):
                    video_id = f"{item_id}:{season}:{ep}"
                    meta["videos"].append({
                        "id": video_id, "title": f"S{season}E{ep} — {title(self.seed, video_id, 0)}",
                        "season": season, "episode": ep, "released": "2024-02-01T00:00:00.000Z",
                        "thumbnail": self.art_url(port, video_id, "still"),
                        "overview": "An episode from the local fixture series.",
                    })
        return meta

    def subtitle_rows(self, item_id: str) -> list[dict]:
        langs = [lang.strip() for lang in self.args.languages.split(",") if lang.strip()]
        rows = []
        for i in range(self.args.subs_per_lang):
            for lang in langs:
                rows.append({
                    "id": f"{item_id}-{lang}-{i}",
                    "url": f"{self.base(self.args.port)}/sub/{urllib.parse.quote(item_id)}/{lang}/{i}.srt",
                    "lang": lang,
                    "movieReleaseName": f"fixture-{item_id}-2024",
                    "subtitleFileName": f"{item_id}.{lang}.{i}.srt",
                })
        return rows

    def stream_rows(self, port: int, item_id: str) -> list[dict]:
        rows = []
        qualities = ("2160p DV", "1080p WEB", "1080p HDTV", "720p WEB", "720p HDTV",
                     "480p WEB", "480p HDTV", "360p")
        for i in range(self.args.sources):
            size = 0.7 + i * 0.37
            peers = 3 + (i * 11) % 97
            label = f"{self.args.addon_name}\n{qualities[i % len(qualities)]}"
            detail = f"{size:.1f} GB • {peers} seeds • fixture-{item_id}.mkv"
            rows.append({
                "name": label, "title": detail, "description": detail,
                "url": f"{self.base(port)}/media/{self.args.media_name}" if self.args.media_name else
                       f"{self.base(port)}/missing/{item_id}.mp4",
                "subtitles": self.subtitle_rows(item_id),
                "behaviorHints": {"videoSize": int(size * 1024 * 1024 * 1024)},
            })
        return rows


class Handler(http.server.BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def setup(self):  # noqa: D102 - stdlib hook
        super().setup()
        self.connection.settimeout(30)

    def log_message(self, format: str, *args: object) -> None:
        if os.environ.get("NUVIOMOCK_TRACE"):
            sys.stderr.write(format % args + "\n")

    @property
    def fixture(self) -> Fixture:
        return self.server.fixture

    def conditioned(self, path: str, query: str) -> bool:
        """Apply a deterministic link profile; True when the request must not be answered."""
        plan = self.server.conditioner.plan(path, query)
        if plan is None:
            return False
        if plan.delay_s:
            time.sleep(plan.delay_s)
        if plan.fail == "503":
            self.send_response(503)
            self.send_header("Content-Length", "0")
            self.end_headers()
            return True
        if plan.fail == "reset":
            self.close_connection = True
            return True
        self.plan = plan
        return False

    def send_body(self, body: bytes, content_type: str, *, condition: bool = True) -> None:
        path, _, query = self.path.partition("?")
        if condition and self.conditioned(path, query):
            return
        plan = getattr(self, "plan", None)
        self.plan = None
        self.send_response(200)
        self.send_header("Content-Type", content_type)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        if plan is not None and plan.kbit:
            link_conditioner.write_throttled(self.wfile.write, body, plan.kbit)
        else:
            self.wfile.write(body)

    def json_reply(self, value: object) -> None:
        self.send_body(json.dumps(value, separators=(",", ":")).encode(), "application/json")

    def do_GET(self) -> None:  # noqa: N802 - BaseHTTPRequestHandler spelling
        raw_path, _, query = self.path.partition("?")
        path = urllib.parse.unquote(raw_path)
        fx = self.fixture
        port = self.server.server_address[1]

        if path == "/manifest.json":
            resources = ["catalog", "meta", "stream", "subtitles"]
            manifest = {
                "id": "org.local.fixture", "version": "1.0.0", "name": fx.args.addon_name,
                "description": "Deterministic hermetic Stremio fixture for Nuvio Native",
                "types": ["movie", "series"], "resources": resources,
                "catalogs": [{"type": t, "id": cid, "name": name} for t, cid, name, _ in fx.catalogs],
            }
            return self.json_reply(manifest)

        match = re.match(r"^/catalog/(movie|series)/([^/]+?)(?:/skip=(\d+))?\.json$", path)
        if match:
            type_, catalog_id, skip = match.group(1), match.group(2), int(match.group(3) or 0)
            rows = next((items for t, cid, _, items in fx.catalogs
                         if t == type_ and cid == catalog_id), [])
            page = rows[skip:skip + fx.args.per_page]
            # Fork Rofocale11: the Home hero reads `background` from a catalog meta; without it
            # the carousel has no art to slide/fade, which made heroosc stall on the fixture.
            return self.json_reply({"metas": [{"id": i["id"], "type": i["type"],
                                               "name": i["name"],
                                               "poster": fx.art_url(port, i["id"], "poster"),
                                               "background": fx.art_url(port, i["id"], "background"),
                                               "description": f"Local fixture entry for {i['id']}.",
                                               "releaseInfo": "2024", "imdbRating": "7.4"}
                                              for i in page]})

        match = re.match(r"^/meta/(movie|series)/(.+)\.json$", path)
        if match:
            item = fx.item_by_id(match.group(2))
            if item is not None:
                return self.json_reply({"meta": fx.meta(port, item)})
            return self.send_error(404)

        match = re.match(r"^/(stream|subtitles)/(movie|series)/(.+)\.json$", path)
        if match:
            resource, type_, item_id = match.groups()
            if item_id.startswith("tt") and ":" in item_id:
                item = {"id": item_id, "type": "series", "name": item_id}
            else:
                item = fx.item_by_id(item_id)
            if item is None:
                return self.send_error(404)
            if resource == "stream":
                return self.json_reply({"streams": fx.stream_rows(port, item_id)})
            return self.json_reply({"subtitles": fx.subtitle_rows(item_id)})

        match = re.match(r"^/art/(poster|background|still)/(.+)\.png$", path)
        if match:
            return self.send_body(fx.art(match.group(2), match.group(1)), "image/png")

        match = re.match(r"^/sub/(.+)/([a-z]+)/(\d+)\.srt$", path)
        if match:
            return self.send_body(srt(f"{match.group(2)} track {match.group(3)}"),
                                  "application/x-subrip", condition=False)

        if path.startswith(f"/media/{fx.args.media_name}") and fx.args.media_name:
            path = f"/{fx.args.media_name}"
        match = re.match(r"^/(.+)$", path)
        if match and fx.args.media and match.group(1) == fx.args.media_name:
            media = fx.args.media
            size = os.path.getsize(media)
            start, end, status = 0, size - 1, 200
            requested = self.headers.get("Range")
            if requested:
                found = re.match(r"bytes=(\d+)-(\d*)", requested)
                if found:
                    start = int(found.group(1))
                    end = int(found.group(2)) if found.group(2) else size - 1
                    status = 206
            length = max(0, end - start + 1)
            self.send_response(status)
            self.send_header("Content-Type", "video/mp4")
            self.send_header("Accept-Ranges", "bytes")
            self.send_header("Content-Length", str(length))
            if status == 206:
                self.send_header("Content-Range", f"bytes {start}-{end}/{size}")
            self.end_headers()
            with open(media, "rb") as source:
                source.seek(start)
                self.wfile.write(source.read(length))
            return

        self.send_error(404)


def legacy_args(argv: list[str]):
    """The documented three positional arguments keep their old meaning."""
    port, imdb_id, mp4 = int(argv[0]), argv[1], os.path.abspath(argv[2])
    ns = argparse.Namespace(port=port, seed=11, movies=0, shows=0, home_hubs=1,
                            section_hubs=0, per_page=100, featured=imdb_id, media=mp4,
                            media_name=os.path.basename(mp4), sources=1, subs_per_lang=0,
                            languages="eng", episodes_per_season=0, seasons=0,
                            cw_art_fixture=False, lead_series=False, link="none", art_w=250,
                            art_h=375, token_len=220, addon_name="Local Fixture")
    ns.legacy = True
    return ns


def parse_args(argv: list[str]):
    if len(argv) == 3 and argv[0].isdigit():
        return legacy_args(argv)
    p = argparse.ArgumentParser(description="Deterministic local Stremio fixture addon")
    p.add_argument("--port", type=int, required=True)
    p.add_argument("--seed", type=int, default=11)
    p.add_argument("--movies", type=int, default=300)
    p.add_argument("--shows", type=int, default=12)
    p.add_argument("--home-hubs", type=int, default=41)
    p.add_argument("--section-hubs", type=int, default=0)
    p.add_argument("--per-page", type=int, default=100)
    p.add_argument("--featured", default="tt33764258")
    p.add_argument("--media", help="local MP4 served with Range support")
    p.add_argument("--sources", type=int, default=8, help="source rows in the detail picker")
    p.add_argument("--subs-per-lang", type=int, default=4)
    p.add_argument("--languages", default="eng,alb,ind,spa")
    p.add_argument("--episodes-per-season", type=int, default=6)
    p.add_argument("--seasons", type=int, default=2)
    p.add_argument("--cw-art-fixture", action="store_true")
    p.add_argument("--lead-series", action="store_true",
                   help="put one series first on Home, so by-id scenes have a known show card")
    p.add_argument("--link", default="none", help="link_conditioner profile, e.g. 3g or remote-wan")
    p.add_argument("--art-w", type=int, default=250)
    p.add_argument("--art-h", type=int, default=375)
    p.add_argument("--token-len", type=int, default=220)
    p.add_argument("--addon-name", default="Local Fixture")
    ns = p.parse_args(argv)
    if ns.media:
        ns.media = os.path.abspath(ns.media)
    ns.media_name = os.path.basename(ns.media) if ns.media else None
    ns.legacy = False
    return ns


def make_conditioner(args) -> link_conditioner.Conditioner:
    klass, spec = link_conditioner.parse_spec(args.link) if any(
        c in args.link for c in ("=", ":")) else (link_conditioner.ALL, args.link)
    profile = link_conditioner.make_profile(spec)
    # Stremio paths have no PMS-style image/listing split: condition both classes alike.
    return link_conditioner.Conditioner(seed=args.seed, profiles={"listing": profile, "image": profile})


def main(argv: list[str] | None = None) -> None:
    args = parse_args(list(sys.argv[1:] if argv is None else argv))
    server = http.server.ThreadingHTTPServer(("127.0.0.1", args.port), Handler)
    server.fixture = Fixture(args)
    server.conditioner = make_conditioner(args)
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        pass
    finally:
        server.server_close()


if __name__ == "__main__":
    main()

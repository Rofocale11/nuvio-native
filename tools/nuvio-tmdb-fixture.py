#!/usr/bin/env python3
"""A hermetic TMDB fixture for the sealed nuviolive host loop.

The fork's TMDB client can be pointed here with `TMDB_API_BASE=http://127.0.0.1:PORT/3` and
`TMDB_IMAGE_BASE=http://127.0.0.1:PORT/t/p` (both read from the environment). Every API answer
and every image byte is generated locally from `--seed`, so detail enrichment, cast pages,
collections, clear logos and trailers run with no external network at all.

    python3 tools/nuvio-tmdb-fixture.py --port 18780 --seed 11
    # TMDB_API_BASE=http://127.0.0.1:18780/3 \
    # TMDB_IMAGE_BASE=http://127.0.0.1:18780/t/p python3 tools/nuvio-sim.py ...

The port is loopback-only. This is a harness fixture, not a product surface.
"""

import argparse
import hashlib
import http.server
import json
import os
import re
import struct
import sys
import zlib


def digest(*parts: object) -> str:
    return hashlib.sha256("|".join(str(p) for p in parts).encode()).hexdigest()


def num(*parts: object, mod: int = 900000) -> int:
    return int(digest(*parts)[:12], 16) % mod + 1


def person_name(seed: int, person_id: int) -> str:
    names = ("Avery Stone", "Jordan Vale", "Mira Chen", "Riley Fox", "Dana Brooks",
             "Samir Haddad", "Nora Ellis", "Theo Marsh", "Lena Park", "Owen Reed")
    return names[num(seed, "person", person_id, mod=len(names)) - 1]


def png_chunk(tag: bytes, data: bytes) -> bytes:
    return struct.pack(">I", len(data)) + tag + data + struct.pack(">I", zlib.crc32(tag + data))


def art_png(seed: int, path: str) -> bytes:
    h = bytes.fromhex(digest(seed, "image", path)[:6])
    rgb = bytes(((h[0] & 0x7F) + 40, (h[1] & 0x7F) + 40, (h[2] & 0x7F) + 40))
    width, height = 300, 450
    row = b"\x00" + rgb * width
    ihdr = struct.pack(">IIBBBBB", width, height, 8, 2, 0, 0, 0)
    return (b"\x89PNG\r\n\x1a\n" + png_chunk(b"IHDR", ihdr)
            + png_chunk(b"IDAT", zlib.compress(row * height, 6)) + png_chunk(b"IEND", b""))


class Fixture:
    def __init__(self, seed: int):
        self.seed = seed

    def title(self, tmdb_id: int, kind: str) -> str:
        lead = ("Crimson", "Silent", "Broken", "Golden", "Hollow", "Distant", "Frozen", "Last")
        tail = ("Harbor", "Meridian", "Lantern", "Circuit", "Orchard", "Signal", "Requiem", "Frontier")
        return f"{lead[num(self.seed, kind, tmdb_id, mod=len(lead)) - 1]} " \
               f"{tail[num(self.seed, kind, tmdb_id, 'b', mod=len(tail)) - 1]}"

    def base_movie(self, tmdb_id: int) -> dict:
        return {
            "id": tmdb_id, "title": self.title(tmdb_id, "movie"),
            "overview": f"Deterministic local TMDB synopsis for movie {tmdb_id}.",
            "release_date": "2024-01-01", "runtime": 108,
            "poster_path": f"/p{tmdb_id}.png", "backdrop_path": f"/b{tmdb_id}.png",
            "genres": [{"id": 18, "name": "Drama"}],
            "belongs_to_collection": {"id": num(self.seed, "collection", tmdb_id),
                                      "name": f"{self.title(tmdb_id, 'movie')} Collection"},
        }

    def base_show(self, tmdb_id: int) -> dict:
        return {
            "id": tmdb_id, "name": self.title(tmdb_id, "show"),
            "overview": f"Deterministic local TMDB synopsis for show {tmdb_id}.",
            "first_air_date": "2024-02-01",
            "poster_path": f"/p{tmdb_id}.png", "backdrop_path": f"/b{tmdb_id}.png",
            "genres": [{"id": 18, "name": "Drama"}],
        }

    def credits(self, tmdb_id: int) -> dict:
        cast = []
        for i in range(4):
            pid = num(self.seed, "cast", tmdb_id, i)
            cast.append({"id": pid, "name": person_name(self.seed, pid),
                         "character": f"Character {i + 1}",
                         "profile_path": f"/person{pid}.png"})
        crew = [{"id": num(self.seed, "crew", tmdb_id), "name": "Fixture Director",
                 "job": "Director", "department": "Directing", "known_for_department": "Directing"}]
        return {"cast": cast, "crew": crew}

    def recommendations(self, tmdb_id: int, kind: str) -> dict:
        rows = []
        for i in range(3):
            rid = num(self.seed, "rec", kind, tmdb_id, i)
            rows.append(self.base_movie(rid) if kind == "movie" else self.base_show(rid))
        return {"results": rows}

    def person(self, person_id: int) -> dict:
        return {
            "id": person_id, "name": person_name(self.seed, person_id),
            "biography": f"Deterministic local biography for person {person_id}.",
            "birthday": "1980-01-01", "place_of_birth": "Fixture City",
            "known_for_department": "Acting", "profile_path": f"/person{person_id}.png",
        }

    def credits_for_person(self, person_id: int) -> dict:
        rows = []
        for i in range(5):
            mid = num(self.seed, "filmography", person_id, i)
            movie = self.base_movie(mid)
            rows.append({**movie, "character": f"Role {i + 1}", "media_type": "movie"})
        return {"cast": rows, "crew": []}

    def videos(self, tmdb_id: int) -> dict:
        return {"results": [{"id": f"trailer{tmdb_id}", "key": "dQw4w9WgXcQ",
                             "site": "YouTube", "name": "Fixture Trailer", "type": "Trailer"}]}

    def images(self, tmdb_id: int) -> dict:
        return {"logos": [{"file_path": f"/logo{tmdb_id}.png", "iso_639_1": "en"}]}

    def details(self, tmdb_id: int, kind: str) -> dict:
        base = self.base_movie(tmdb_id) if kind == "movie" else self.base_show(tmdb_id)
        return {**base, "credits": self.credits(tmdb_id),
                "recommendations": self.recommendations(tmdb_id, kind),
                "images": self.images(tmdb_id), "videos": self.videos(tmdb_id)}

    def find(self, imdb_id: str) -> dict:
        suffix = re.sub(r"\D", "", imdb_id)
        tmdb_id = int(suffix) % 900000 + 1 if suffix else 1
        show = suffix.startswith("8")
        return {"movie_results": [] if show else [self.base_movie(tmdb_id)],
                "tv_results": [self.base_show(tmdb_id)] if show else []}


class Handler(http.server.BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, format: str, *args: object) -> None:
        if os.environ.get("NUVIOTMDB_TRACE"):
            sys.stderr.write(format % args + "\n")

    def reply(self, value: object, content_type: str = "application/json") -> None:
        body = value if isinstance(value, bytes) else json.dumps(value, separators=(",", ":")).encode()
        self.send_response(200)
        self.send_header("Content-Type", content_type)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self) -> None:  # noqa: N802 - BaseHTTPRequestHandler spelling
        path = self.path.split("?", 1)[0]
        fx = self.server.fixture
        match = re.match(r"^/3/find/(tt\d+)$", path)
        if match:
            return self.reply(fx.find(match.group(1)))
        match = re.match(r"^/3/(movie|tv)/(\d+)(?:/(credits|videos|recommendations|external_ids))?$", path)
        if match:
            kind, tmdb_id, sub = match.group(1), int(match.group(2)), match.group(3)
            if sub == "credits":
                return self.reply(fx.credits(tmdb_id))
            if sub == "videos":
                return self.reply(fx.videos(tmdb_id))
            if sub == "recommendations":
                return self.reply(fx.recommendations(tmdb_id, kind))
            if sub == "external_ids":
                return self.reply({"imdb_id": f"tt{tmdb_id:07d}"})
            return self.reply(fx.details(tmdb_id, kind))
        match = re.match(r"^/3/person/(\d+)(?:/(combined_credits))?$", path)
        if match:
            pid = int(match.group(1))
            return self.reply(fx.credits_for_person(pid) if match.group(2) else fx.person(pid))
        match = re.match(r"^/3/collection/(\d+)$", path)
        if match:
            cid = int(match.group(1))
            return self.reply({"id": cid, "name": f"Fixture Collection {cid}",
                               "overview": "Local collection fixture.",
                               "parts": [fx.base_movie(num(fx.seed, "part", cid, i)) for i in range(3)]})
        match = re.match(r"^/t/p/(w\d+)/([A-Za-z0-9._-]+)$", path)
        if match:
            return self.reply(art_png(fx.seed, match.group(2)), "image/png")
        self.send_error(404)


def main(argv: list[str] | None = None) -> None:
    p = argparse.ArgumentParser(description="Loopback TMDB API + image fixture")
    p.add_argument("--port", type=int, required=True)
    p.add_argument("--seed", type=int, default=11)
    args = p.parse_args(list(sys.argv[1:] if argv is None else argv))
    server = http.server.ThreadingHTTPServer(("127.0.0.1", args.port), Handler)
    server.fixture = Fixture(args.seed)
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        pass
    finally:
        server.server_close()


if __name__ == "__main__":
    main()

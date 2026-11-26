#!/usr/bin/env python3
"""A hermetic local Stremio addon for the nuviolive host loop.

Serves a real HTTP stream for one Cinemeta movie so the player has a genuine ratingKey and
metadata. That is what the subtitle drill-down and the TMDB clear logo need: a `playurl` file has
no ratingKey, so subtitle search can never resolve against it. No torrent, debrid key, account or
TV is involved.

    python3 tools/nuvio-fixture-addon.py PORT tt33764258 /path/to/local.mp4

Then put `http://127.0.0.1:PORT` in a private `--addons` list alongside Cinemeta and
OpenSubtitles. Serve only on loopback; kill the process when the scenario is done.
"""

import http.server
import json
import os
import re
import sys


def main() -> None:
    if len(sys.argv) != 4:
        raise SystemExit("usage: nuvio-fixture-addon.py PORT IMDb_ID LOCAL_MP4")
    port, imdb_id, mp4 = int(sys.argv[1]), sys.argv[2], os.path.abspath(sys.argv[3])
    name = os.path.basename(mp4)
    size = os.path.getsize(mp4)
    manifest = {
        "id": "org.local.fixture",
        "version": "1.0.0",
        "name": "Local Fixture",
        "types": ["movie"],
        "resources": [{"name": "stream", "types": ["movie"], "idPrefixes": ["tt"]}],
    }

    class Handler(http.server.BaseHTTPRequestHandler):
        protocol_version = "HTTP/1.1"

        def log_message(self, *_args: object) -> None:
            pass

        def json_reply(self, value: object) -> None:
            body = json.dumps(value).encode()
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)

        def do_GET(self) -> None:  # noqa: N802 - BaseHTTPRequestHandler spelling
            path = self.path.split("?", 1)[0]
            if path == "/manifest.json":
                return self.json_reply(manifest)
            if path == f"/stream/movie/{imdb_id}.json":
                return self.json_reply({"streams": [{
                    "name": "Local fixture",
                    "title": f"{name} (local fixture)",
                    "url": f"http://127.0.0.1:{port}/{name}",
                }]})
            if path == f"/{name}":
                start, end, status = 0, size - 1, 200
                requested = self.headers.get("Range")
                if requested:
                    match = re.match(r"bytes=(\d+)-(\d*)", requested)
                    if match:
                        start = int(match.group(1))
                        end = int(match.group(2)) if match.group(2) else size - 1
                        status = 206
                length = max(0, end - start + 1)
                self.send_response(status)
                self.send_header("Content-Type", "video/mp4")
                self.send_header("Accept-Ranges", "bytes")
                self.send_header("Content-Length", str(length))
                if status == 206:
                    self.send_header("Content-Range", f"bytes {start}-{end}/{size}")
                self.end_headers()
                with open(mp4, "rb") as source:
                    source.seek(start)
                    self.wfile.write(source.read(length))
                return
            self.send_error(404)

    http.server.ThreadingHTTPServer(("127.0.0.1", port), Handler).serve_forever()


if __name__ == "__main__":
    main()

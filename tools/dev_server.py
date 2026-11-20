#!/usr/bin/env python
"""Local stand-in for Vercel: serves web/dist and forwards /api/<name> to a local Rust function.

Usage:  python tools/dev_server.py [--port 8787] [--map health=3101 --map auth_login=3105 ...]
Each function binary must be started with VERCEL_DEV_PORT=<its port> (see tools/smoke_test.py).
GET and POST are forwarded with their body, Cookie/Origin/Host headers; every response header of the
function (including Set-Cookie) comes back unchanged.
Also serves /__smoke, a page that calls the API through the same JS glue the app uses.
"""
from __future__ import annotations

import argparse
import http.client
import http.server
import json
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
DIST = ROOT / "web" / "dist"

SMOKE_PAGE = """<!doctype html><meta charset="utf-8"><title>smoke</title>
<pre id="out">running</pre>
<script src="/lifeui.js"></script>
<script>
(async () => {
  const health = JSON.parse(await lifeui.call_api('/api/health'));
  const db = JSON.parse(await lifeui.call_api('/api/db_ping'));
  const env = JSON.parse(lifeui.environment());
  document.getElementById('out').textContent = JSON.stringify({ health, db, env });
  document.title = 'done';
})();
</script>"""

FORWARD_REQUEST_HEADERS = {"authorization", "cookie", "content-type", "origin", "host", "accept", "user-agent", "x-forwarded-for"}  # Vercel sets x-forwarded-for itself; here tests may
HOP_BY_HOP = {"connection", "keep-alive", "transfer-encoding", "te", "trailer", "upgrade", "content-length"}


class Handler(http.server.SimpleHTTPRequestHandler):
    routes: dict[str, int] = {}
    # Windows' registry-based MIME table lacks several types; set them explicitly, as Vercel would.
    extensions_map = {
        **http.server.SimpleHTTPRequestHandler.extensions_map,
        ".html": "text/html; charset=utf-8",
        ".js": "text/javascript; charset=utf-8",
        ".wasm": "application/wasm",
        ".webmanifest": "application/manifest+json",
        ".png": "image/png",
        ".json": "application/json",
        ".css": "text/css; charset=utf-8",
        "": "application/octet-stream",
    }

    def __init__(self, *args, **kwargs):
        super().__init__(*args, directory=str(DIST), **kwargs)

    def log_message(self, *args):  # keep the console quiet
        pass

    def end_headers(self):
        self.send_header("Cache-Control", "no-store")
        super().end_headers()

    def _send(self, status: int, body: bytes, content_type: str) -> None:
        self.send_response(status)
        self.send_header("Content-Type", content_type)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self):
        path = self.path.split("?")[0]
        if path == "/__smoke":
            self._send(200, SMOKE_PAGE.encode(), "text/html; charset=utf-8")
        elif path.startswith("/api/"):
            self._proxy("GET", path)
        else:
            super().do_GET()

    def do_POST(self):
        path = self.path.split("?")[0]
        if path.startswith("/api/"):
            self._proxy("POST", path)
        else:
            self._send(405, b"POST only for /api/*", "text/plain")

    def _proxy(self, method: str, path: str) -> None:
        target = self.path
        rewrite = REWRITES.get(path.rstrip("/"))
        if rewrite:  # same rule as Vercel (vercel.json "rewrites"): e.g. /api/runner/news -> /api/runner?route=news
            query = self.path.partition("?")[2]
            target = rewrite + (("&" if "?" in rewrite else "?") + query if query else "")
            path = rewrite.split("?")[0]
        name = path[len("/api/"):].strip("/")
        port = self.routes.get(name)
        if port is None:
            self._send(404, json.dumps({"ok": False, "error": f"no function '{name}'"}).encode(), "application/json")
            return
        length = int(self.headers.get("Content-Length") or 0)
        body = self.rfile.read(length) if length else None
        headers = {k: v for k, v in self.headers.items() if k.lower() in FORWARD_REQUEST_HEADERS}
        try:
            conn = http.client.HTTPConnection("127.0.0.1", port, timeout=40)
            conn.request(method, target, body=body, headers=headers)
            response = conn.getresponse()
            data = response.read()
            self.send_response(response.status)
            for key, value in response.getheaders():  # keeps repeated headers such as Set-Cookie
                if key.lower() not in HOP_BY_HOP:
                    self.send_header(key, value)
            self.send_header("Content-Length", str(len(data)))
            self.end_headers()
            self.wfile.write(data)
        except Exception:
            self._send(502, json.dumps({"ok": False, "error": "function not reachable"}).encode(), "application/json")


def load_rewrites() -> dict[str, str]:
    """Exact-path rewrites from vercel.json (the only kind this app uses)."""
    try:
        config = json.loads((Path(__file__).resolve().parents[1] / "vercel.json").read_text(encoding="utf-8"))
    except (OSError, ValueError):
        return {}
    return {r["source"].rstrip("/"): r["destination"] for r in config.get("rewrites", []) if ":" not in r["source"]}


REWRITES = load_rewrites()


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--port", type=int, default=8787)
    parser.add_argument("--map", action="append", default=[], help="name=port, e.g. health=3101")
    args = parser.parse_args()
    for item in args.map:
        name, port = item.split("=")
        Handler.routes[name] = int(port)
    server = http.server.ThreadingHTTPServer(("127.0.0.1", args.port), Handler)
    print(f"dev server on http://127.0.0.1:{args.port}  routes={Handler.routes}", flush=True)
    server.serve_forever()


if __name__ == "__main__":
    main()

#!/usr/bin/env python3
# Created: 2026-09-07 by Constructor Tech
"""Serve the sandbox page and reverse-proxy the settings API from the same origin.

Standard library only. Static: `/` and `/app.js`. Everything under
`/settings-service/` and `/openapi.json` is forwarded verbatim to the example
server, including the headers the API contract relies on (Authorization,
If-Match, X-Step-Up-Token) and, on the way back, the ones a client must read
(ETag, WWW-Authenticate, Location).
"""

import http.server
import os
import sys
import urllib.error
import urllib.request

UPSTREAM = os.environ.get("SETTINGS_UPSTREAM", "http://127.0.0.1:8087").rstrip("/")
PORT = int(os.environ.get("SANDBOX_PORT", "8090"))
HERE = os.path.dirname(os.path.abspath(__file__))

PROXIED = ("/settings-service/", "/openapi.json")
FORWARD_REQUEST = {"authorization", "content-type", "accept", "if-match", "x-step-up-token", "x-request-id"}
FORWARD_RESPONSE = {"content-type", "etag", "www-authenticate", "location"}
STATIC = {"/": ("index.html", "text/html; charset=utf-8"), "/index.html": ("index.html", "text/html; charset=utf-8"), "/app.js": ("app.js", "text/javascript; charset=utf-8")}


class Handler(http.server.BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def _static(self, name, content_type):
        with open(os.path.join(HERE, name), "rb") as f:
            payload = f.read()
        self.send_response(200)
        self.send_header("Content-Type", content_type)
        self.send_header("Cache-Control", "no-store")
        self.send_header("Content-Length", str(len(payload)))
        self.end_headers()
        self.wfile.write(payload)

    def _proxy(self):
        length = int(self.headers.get("Content-Length") or 0)
        body = self.rfile.read(length) if length else None
        headers = {k: v for k, v in self.headers.items() if k.lower() in FORWARD_REQUEST}
        request = urllib.request.Request(UPSTREAM + self.path, data=body, method=self.command, headers=headers)
        try:
            with urllib.request.urlopen(request, timeout=30) as response:
                status, response_headers, payload = response.status, response.headers, response.read()
        except urllib.error.HTTPError as error:
            status, response_headers, payload = error.code, error.headers, error.read()
        except urllib.error.URLError as error:
            payload = ('{"title":"Bad Gateway","status":502,"detail":"upstream %s unreachable: %s"}' % (UPSTREAM, error.reason)).encode()
            status, response_headers = 502, {"Content-Type": "application/problem+json"}
        self.send_response(status)
        for key, value in response_headers.items():
            if key.lower() in FORWARD_RESPONSE:
                self.send_header(key, value)
        self.send_header("Content-Length", str(len(payload)))
        self.end_headers()
        self.wfile.write(payload)

    def _route(self):
        path = self.path.split("?", 1)[0]
        if self.command == "GET" and path in STATIC:
            name, content_type = STATIC[path]
            self._static(name, content_type)
        elif self.path.startswith(PROXIED):
            self._proxy()
        else:
            self.send_response(404)
            self.send_header("Content-Length", "0")
            self.end_headers()

    do_GET = do_POST = do_PUT = do_PATCH = do_DELETE = _route

    def log_message(self, fmt, *args):  # noqa: D102 - one concise line per request
        sys.stderr.write("%s %s\n" % (self.command, self.path))


if __name__ == "__main__":
    server = http.server.ThreadingHTTPServer(("127.0.0.1", PORT), Handler)
    print("sandbox on http://127.0.0.1:%d/ -> %s" % (PORT, UPSTREAM), flush=True)
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        pass

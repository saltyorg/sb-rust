"""The fake GitHub and SVM proxy of upgrade.sh, inside its container.

It listens on 127.0.0.1:443 with a certificate for the hosts the
container's /etc/hosts sends here (svm.saltbox.dev, api.github.com,
github.com, objects.githubusercontent.com), signed by the throwaway CA the
container's system store trusts. It answers the release lists of the
repositories releases.json names (saltyorg/sb-go and saltyorg/sb-rust,
or sb-rust alone; rewritten by the test between its phases) with that
file's releases, through the proxy (`/version?url=<GitHub API URL>`) and
directly, and serves the assets under
`/saltyorg/sb-rust/releases/download/<tag>/<name>` from the assets
directory. Every other GET or HEAD is passed on to the real host, at the
address it had before /etc/hosts was changed (real.json), with its own
name for TLS: saltbox.fact's and uv's lookups go through as they would;
a request that can't be passed on is a 502 naming why. Each request is
logged to requests.log, one line each.

    python3 fake-github.py DIR

DIR holds cert.pem, key.pem, releases.json, real.json and assets/.
"""

from __future__ import annotations

import http.client
import http.server
import json
import re
import socket
import ssl
import sys
import urllib.parse
from pathlib import Path
from typing import Any

DIR = Path(sys.argv[1])
RELEASES = re.compile(r"^/repos/saltyorg/([^/]+)/releases(/latest)?/?$")
ASSET = re.compile(r"^/saltyorg/sb-rust/releases/download/([^/]+)/([^/]+)$")
# The system store, which holds the test's CA too: a forwarded request
# reaches the real host and checks its real certificate.
SYSTEM_CA = "/etc/ssl/certs/ca-certificates.crt"
# Hop-by-hop headers (RFC 9110, 7.6.1), and the ones this server writes
# itself: neither is passed on.
HOP = {
    "connection",
    "keep-alive",
    "proxy-authenticate",
    "proxy-authorization",
    "proxy-connection",
    "te",
    "trailer",
    "transfer-encoding",
    "upgrade",
}
OWN = {"content-length", "date", "server"}


def listed() -> tuple[list[str], list[dict[str, Any]]]:
    """Return the repositories answered and their releases, newest first."""
    data: dict[str, Any] = json.loads(
        (DIR / "releases.json").read_text(encoding="utf-8")
    )
    repos = [str(repo) for repo in data["repos"]]
    releases = [dict(release) for release in data["releases"]]
    return repos, releases


def real(host: str) -> str | None:
    """Return `host`'s real address, resolved before /etc/hosts changed."""
    addresses: dict[str, str] = json.loads(
        (DIR / "real.json").read_text(encoding="utf-8")
    )
    return addresses.get(host)


class Handler(http.server.BaseHTTPRequestHandler):
    """The fake's answers, and the requests it passes on."""

    protocol_version = "HTTP/1.1"

    def log_message(self, format: str, *args: object) -> None:
        """Log one line per request in requests.log (the base class's name)."""
        headers = getattr(self, "headers", None)
        host = headers.get("Host", "-") if headers else "-"
        with (DIR / "requests.log").open("a", encoding="utf-8") as log:
            log.write(f"{host} {format % args}\n")

    def send(self, code: int, body: bytes, kind: str = "application/json") -> None:
        """Answer with `body`, its length given."""
        self.send_response(code)
        self.send_header("Content-Type", kind)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        if self.command != "HEAD":
            self.wfile.write(body)

    def answer(self, path: str) -> bytes | None:
        """Return the fake's own answer for a GitHub API path, or None."""
        match = RELEASES.match(path)
        if not match:
            return None
        repos, releases = listed()
        if match.group(1) not in repos:
            return None
        if match.group(2):
            stable = [r for r in releases if not r["prerelease"] and not r["draft"]]
            return json.dumps(stable[0] if stable else {}).encode()
        return json.dumps(releases).encode()

    def do_GET(self) -> None:
        """Answer the release lists and the assets; pass the rest on."""
        host = self.headers.get("Host", "").split(":")[0]
        parsed = urllib.parse.urlsplit(self.path)
        if host == "svm.saltbox.dev" and parsed.path == "/version":
            asked = urllib.parse.parse_qs(parsed.query).get("url", [""])[0]
            target = urllib.parse.urlsplit(asked)
            if target.hostname == "api.github.com":
                body = self.answer(target.path)
                if body is not None:
                    self.send(200, body)
                    return
        if host == "api.github.com":
            body = self.answer(parsed.path)
            if body is not None:
                self.send(200, body)
                return
        if host == "github.com":
            match = ASSET.match(parsed.path)
            if match:
                path = DIR / "assets" / match.group(1) / match.group(2)
                if path.is_file():
                    self.send(200, path.read_bytes(), "application/octet-stream")
                else:
                    self.send(404, b"Not Found", "text/plain")
                return
        self.forward(host)

    do_HEAD = do_GET

    def forward(self, host: str) -> None:
        """Pass the request on to the real `host`, its answer as given."""
        address = real(host)
        if not address:
            self.send(502, b"no real address", "text/plain")
            return
        context = ssl.create_default_context(cafile=SYSTEM_CA)
        conn = http.client.HTTPSConnection(host, 443, context=context, timeout=60)
        try:
            sock = socket.create_connection((address, 443), timeout=60)
            try:
                conn.sock = context.wrap_socket(sock, server_hostname=host)
            except BaseException:
                sock.close()
                raise
            headers = [(k, v) for k, v in self.headers.items() if k.lower() not in HOP]
            conn.putrequest(self.command, self.path, skip_host=True)
            for key, value in headers:
                conn.putheader(key, value)
            conn.endheaders()
            resp = conn.getresponse()
            body = resp.read()
        except (OSError, http.client.HTTPException) as err:
            self.send(502, f"forwarding to {host} failed: {err}".encode(), "text/plain")
            return
        finally:
            conn.close()
        self.send_response(resp.status)
        for key, value in resp.getheaders():
            if key.lower() not in HOP | OWN:
                self.send_header(key, value)
        if self.command == "HEAD":
            # A HEAD's answer has no body: the real length, as given.
            length = resp.getheader("Content-Length")
            if length is not None:
                self.send_header("Content-Length", length)
        elif resp.status not in (204, 304):
            self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        if self.command != "HEAD":
            self.wfile.write(body)


def main() -> None:
    """Serve on 127.0.0.1:443 until the container ends."""
    server = http.server.ThreadingHTTPServer(("127.0.0.1", 443), Handler)
    tls = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    tls.load_cert_chain(DIR / "cert.pem", DIR / "key.pem")
    server.socket = tls.wrap_socket(server.socket, server_side=True)
    server.serve_forever()


if __name__ == "__main__":
    main()

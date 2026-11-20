#!/usr/bin/env python3
"""Serve the browser client and its isolated application origins locally.

The shell/client origin is intentionally separate from the Home Assistant and
OpenCode application origins. Each app origin gets its own service worker so
root-relative app paths do not collide with the Orqella shell.
"""

from __future__ import annotations

import argparse
import functools
import http.server
import re
import threading
from pathlib import Path

ROOT = Path(__file__).resolve().parent
APP_PORTS = {"homeassistant": 8810, "opencode": 8811}


class Handler(http.server.SimpleHTTPRequestHandler):
    def end_headers(self) -> None:
        path = self.path.split("?", 1)[0]
        if path == "/client" or path.startswith("/client/"):
            self.send_header("Content-Security-Policy", "frame-ancestors 'none'")
        elif path == "/app-client" or path.startswith("/app-client/"):
            self.send_header(
                "Content-Security-Policy",
                "frame-ancestors 'self' http://*.orqella.localhost:* http://127.0.0.1:*",
            )
        elif path == "/module-bootstrap.html":
            self.send_header(
                "Content-Security-Policy",
                "default-src 'self'; script-src 'self' 'unsafe-inline' 'unsafe-eval'; "
                "style-src 'self' 'unsafe-inline'; img-src 'self' data: blob:; "
                "font-src 'self' data:; media-src 'self' blob: data:; "
                "connect-src 'self'; worker-src 'self' blob:; frame-src 'self'; "
                "base-uri 'self'; form-action 'self'; "
                "frame-ancestors 'self' http://orqella.localhost:* http://*.orqella.localhost:* http://127.0.0.1:* http://localhost:*",
            )
        super().end_headers()

    def log_message(self, fmt: str, *args: object) -> None:
        message = fmt % args
        message = re.sub(
            r'([?&](?:auth_token|token|__orqella_ws)=)[^& ]+',
            r'\1[redacted]',
            message,
        )
        print(f"[{self.server.server_port}] {message}", flush=True)


def serve(bind: str, port: int) -> http.server.ThreadingHTTPServer:
    handler = functools.partial(Handler, directory=str(ROOT))
    server = http.server.ThreadingHTTPServer((bind, port), handler)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    return server


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--bind", default="127.0.0.1")
    parser.add_argument("--shell-port", type=int, default=8801)
    args = parser.parse_args()

    servers = [serve(args.bind, args.shell_port)]
    servers.extend(serve(args.bind, port) for port in APP_PORTS.values())
    print(f"Shell client: http://{args.bind}:{args.shell_port}/client/", flush=True)
    for name, port in APP_PORTS.items():
        print(f"{name} app origin: http://{args.bind}:{port}/app-client/", flush=True)
    try:
        threading.Event().wait()
    except KeyboardInterrupt:
        for server in servers:
            server.shutdown()


if __name__ == "__main__":
    main()

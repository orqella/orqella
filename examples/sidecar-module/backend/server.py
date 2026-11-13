import json, os
from http.server import BaseHTTPRequestHandler, HTTPServer

TOKEN = os.environ.get("ORQELLA_MODULE_TOKEN", "")

class Handler(BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass
    def do_GET(self):
        if self.headers.get("X-Orqella-Module-Token") != TOKEN:
            self.send_response(401); self.end_headers(); return
        if self.path == "/health":
            body = {"ok": True}
        elif self.path == "/api/quote":
            body = {"quote": "Small tools, sharply made.", "machine": os.uname().nodename}
        else:
            self.send_response(404); self.end_headers(); return
        data = json.dumps(body).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

port = int(os.environ["ORQELLA_MODULE_PORT"])
HTTPServer(("127.0.0.1", port), Handler).serve_forever()

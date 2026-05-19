"""Vanilla Python http.server. Standard library; no async. The
slow baseline — Django/Flask add framework overhead on top of
this. Matches what most Python TFB entries look like before
async/uvloop tuning."""

from http.server import HTTPServer, BaseHTTPRequestHandler
from threading import Thread
from socketserver import ThreadingMixIn
import sys

BODY = b"Hello, World!"


class Handler(BaseHTTPRequestHandler):
    # Default is HTTP/1.0 — closes after every request, so a
    # keep-alive client gets one reply then disconnects. Bench
    # measures throughput, not handshake latency; force 1.1.
    protocol_version = "HTTP/1.1"

    def log_message(self, *a, **kw):
        pass  # silence per-request stderr noise

    def do_GET(self):
        if self.path == "/plain":
            self.send_response(200)
            self.send_header("Content-Type", "text/plain")
            self.send_header("Content-Length", str(len(BODY)))
            self.end_headers()
            self.wfile.write(BODY)
        else:
            self.send_response(404)
            self.end_headers()


class TServer(ThreadingMixIn, HTTPServer):
    daemon_threads = True


port = int(sys.argv[1]) if len(sys.argv) > 1 else 0
srv = TServer(("127.0.0.1", port), Handler)
print(f"python on 127.0.0.1:{srv.server_address[1]}", flush=True)
srv.serve_forever()

#!/usr/bin/env python3
"""Tiny HTTPS reverse proxy for browser tests; never used in production."""

import http.client
import http.server
import ssl
import sys

listen_port = int(sys.argv[3]) if len(sys.argv) > 3 else 12449
upstream_port = int(sys.argv[4]) if len(sys.argv) > 4 else 12448
public_host = sys.argv[5] if len(sys.argv) > 5 else "127.0.0.1:12449"


class Proxy(http.server.BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def proxy(self):
        length = int(self.headers.get("Content-Length", "0"))
        body = self.rfile.read(length) if length else None
        headers = {key: value for key, value in self.headers.items()
                   if key.lower() not in {"host", "connection", "content-length", "x-forwarded-proto"}}
        headers["Host"] = public_host
        headers["X-Forwarded-Proto"] = "https"
        upstream = http.client.HTTPConnection("127.0.0.1", upstream_port, timeout=20)
        try:
            upstream.request(self.command, self.path, body=body, headers=headers)
            response = upstream.getresponse()
            payload = response.read()
            self.send_response(response.status)
            for key, value in response.getheaders():
                if key.lower() not in {"transfer-encoding", "connection", "content-length"}:
                    self.send_header(key, value)
            self.send_header("Content-Length", str(len(payload)))
            self.send_header("Connection", "close")
            self.end_headers()
            self.wfile.write(payload)
            self.close_connection = True
        finally:
            upstream.close()

    do_GET = do_HEAD = do_POST = do_PUT = do_DELETE = proxy


context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
context.load_cert_chain(sys.argv[1], sys.argv[2])
server = http.server.ThreadingHTTPServer(("127.0.0.1", listen_port), Proxy)
server.socket = context.wrap_socket(server.socket, server_side=True)
server.serve_forever()

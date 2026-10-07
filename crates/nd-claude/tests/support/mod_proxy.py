"""Fault timing only: forward real mod HTTP verbatim over two private Unix sockets."""
import http.client
import http.server
from pathlib import Path
import socket
import socketserver
import sys
import time

listen, upstream, gates = sys.argv[1:]
gates = Path(gates)

class UnixHTTP(http.client.HTTPConnection):
    def connect(self):
        self.sock = socket.socket(socket.AF_UNIX)
        self.sock.connect(upstream)

class Handler(http.server.BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass
    def forward(self):
        gate = 'next' if self.path.startswith('/next?') else 'result' if self.path.startswith('/result/') else None
        if gate:
            (gates / (gate + '-seen')).touch()
            while (gates / ('hold-' + gate)).exists():
                time.sleep(.01)
        body = self.rfile.read(int(self.headers.get('Content-Length', '0')))
        client = UnixHTTP('localhost', timeout=30)
        try:
            headers = dict(self.headers)
            headers['Connection'] = 'close'
            client.request(self.command, self.path, body=body, headers=headers)
            response = client.getresponse()
            data = response.read()
            self.send_response(response.status)
            self.send_header('Content-Type', response.getheader('Content-Type', 'application/json'))
            self.send_header('Content-Length', str(len(data)))
            self.send_header('Connection', 'close')
            self.end_headers()
            self.wfile.write(data)
        finally:
            client.close()
        self.close_connection = True
    do_GET = forward
    do_POST = forward

class Server(socketserver.ThreadingMixIn, socketserver.UnixStreamServer):
    daemon_threads = True

with Server(listen, Handler) as server:
    server.serve_forever()

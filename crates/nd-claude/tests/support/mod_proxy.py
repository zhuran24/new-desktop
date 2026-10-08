"""Fault timing only: forward real mod HTTP verbatim over two private Unix sockets."""
import http.client
import http.server
import json
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
        body = self.rfile.read(int(self.headers.get('Content-Length', '0')))
        target = gates / 'drop-result'
        discard = False
        if gate == 'result' and target.exists():
            expected = json.loads(target.read_text())
            discard = (self.path.endswith('/' + expected['op_id'])
                       and json.loads(body)['mod_gen'] == expected['mod_gen'])
            if discard:
                (gates / 'target-result-seen').touch()
        if gate:
            (gates / (gate + '-seen')).touch()
            # Only the selected op in the selected generation may be held.
            # Other results must drain so the mod can reach the target Ping.
            while (gates / ('hold-' + gate)).exists() and (gate != 'result' or discard):
                time.sleep(.01)
        if discard:
            (gates / 'result-dropped').touch()
            self.send_response(200)
            self.send_header('Content-Length', '2')
            self.send_header('Connection', 'close')
            self.end_headers()
            self.wfile.write(b'{}')
            self.close_connection = True
            return
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

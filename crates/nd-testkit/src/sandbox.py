"""Process/output supervisor inside bwrap; never emulates a backend protocol."""
import json
import os
from pathlib import Path
import subprocess
import selectors
import socket
import socketserver
import sys
import threading


class Proxy(socketserver.BaseRequestHandler):
    def handle(self):
        # Only a Unix socket is reachable outside this private network namespace.
        with socket.socket(socket.AF_UNIX) as upstream, selectors.DefaultSelector() as poll:
            upstream.connect('/sandbox/model.sock')
            poll.register(self.request, selectors.EVENT_READ, upstream)
            poll.register(upstream, selectors.EVENT_READ, self.request)
            while poll.get_map():
                for key, _ in poll.select():
                    data = key.fileobj.recv(65536)
                    if data:
                        key.data.sendall(data)
                    else:
                        poll.unregister(key.fileobj)
                        key.data.shutdown(socket.SHUT_WR)


class Server(socketserver.ThreadingTCPServer):
    allow_reuse_address = True
    daemon_threads = True

config = json.loads(Path(sys.argv[1]).read_text())
stem = Path('/sandbox/out') / config['name']
env = dict(os.environ)
if config.get('claude'):
    server = Server(('127.0.0.1', 8765), Proxy)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    env.update(ANTHROPIC_BASE_URL='http://127.0.0.1:8765', ANTHROPIC_API_KEY='offline-fixture',
               DISABLE_AUTOUPDATER='1', DISABLE_TELEMETRY='1', DISABLE_ERROR_REPORTING='1',
               CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC='1', CLAUDE_CODE_EAGER_FLUSH='1',
               CLAUDE_CODE_SDK_READS_SESSION_STATE='1', TERM='dumb')
with stem.with_suffix('.stdout').open('wb') as stdout, stem.with_suffix('.stderr').open('wb') as stderr:
    process = subprocess.Popen(['/program', *config['args']], stdin=subprocess.DEVNULL,
                               stdout=stdout, stderr=stderr, env=env)
    code = process.wait()
temp = stem.with_suffix('.tmp')
temp.write_text(str(code))
temp.rename(stem.with_suffix('.exit'))
sys.exit(0 if code == 0 else 1)

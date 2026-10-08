#!/usr/bin/env python3
"""Generate records with the pinned real CLI in a disposable network namespace.
Only the Messages API is replaced. No transcript is created or edited by this script.
"""
import hashlib
import http.server
import json
from pathlib import Path
import queue
import shutil
import subprocess
import sys
import threading
import time
import uuid

CLI = Path('/cli')


def outside():
    global CLI
    sys.path.insert(0, str(Path(__file__).resolve().parents[3] / 'nd-testkit/python'))
    from isolation import Sandbox, PINNED_CLI, environment
    CLI = PINNED_CLI
    destination = Path(sys.argv[1]).resolve()
    destination.mkdir(parents=True, exist_ok=True)
    assert not any(destination.iterdir()), 'evidence destination must be empty'
    with Sandbox('records', memory_max=12 * 1024**3, temporary_parent='/mnt/wd_external/nd-build/tmp', mount='/fixture') as box:
        root = box.root
        shutil.copyfile(__file__, root / 'generate.py')
        (root / 'environment.json').write_text(json.dumps(environment('/fixture', claude=True, CLAUDE_CONFIG_DIR='/fixture/config')))
        (root / 'out/isolation.json').write_text(json.dumps(dict(slice=box.slice, **box.limits()), indent=2) + '\n')
        try:
            subprocess.run(box.command(['/usr/bin/python3', '/fixture/generate.py', '--inside'], bindings=[(CLI, '/cli')], runtime_max=180), check=True)
        finally:
            for file in (root / 'out').iterdir():
                shutil.copyfile(file, destination / file.name)
    manifest = json.loads((destination / 'manifest.json').read_text())
    manifest['cli_sha256'] = hashlib.file_digest(CLI.open('rb'), 'sha256').hexdigest()
    manifest['isolation'] = 'shared nd-testkit Sandbox; bwrap --unshare-all; temporary HOME/config/XDG; transient slice; MemoryMax=12G; no host home or run mount'
    manifest['cleanup'] = {'temporary_directory_removed': not root.exists(), 'units_inactive': True, 'unit': box.unit, 'slice': box.slice}
    (destination / 'manifest.json').write_text(json.dumps(manifest, indent=2) + '\n')
    print(json.dumps(manifest, indent=2))


class Model(http.server.BaseHTTPRequestHandler):
    requests = []

    def log_message(self, *_):
        pass

    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
        self.requests.append(body)
        if 'count_tokens' in self.path:
            content = json.dumps({'input_tokens': 100}).encode()
            self.send_response(200)
            self.send_header('Content-Length', str(len(content)))
            self.end_headers()
            self.wfile.write(content)
            return
        number = len(self.requests)
        text = f'OFFLINE_REPLY_{number}'
        # A normal summary response to the real CLI's compact request.
        if 'summary' in json.dumps(body.get('messages', [])[-1:]).lower():
            text = f'<summary>OFFLINE_SUMMARY_{number}: fixture context retained.</summary>'
        message = {'id': f'msg_fixture_{number}', 'type': 'message', 'role': 'assistant',
                   'model': body['model'], 'content': [{'type': 'text', 'text': text}],
                   'stop_reason': 'end_turn', 'stop_sequence': None,
                   'usage': {'input_tokens': 100, 'output_tokens': 20}}
        if not body.get('stream'):
            content = json.dumps(message).encode()
            self.send_response(200)
            self.send_header('Content-Type', 'application/json')
            self.send_header('Content-Length', str(len(content)))
            self.end_headers()
            self.wfile.write(content)
            return
        self.send_response(200)
        self.send_header('Content-Type', 'text/event-stream')
        self.end_headers()
        events = [
            ('message_start', {'message': {**message, 'content': [], 'stop_reason': None}}),
            ('content_block_start', {'index': 0, 'content_block': {'type': 'text', 'text': ''}}),
            ('content_block_delta', {'index': 0, 'delta': {'type': 'text_delta', 'text': text}}),
            ('content_block_stop', {'index': 0}),
            ('message_delta', {'delta': {'stop_reason': 'end_turn', 'stop_sequence': None}, 'usage': {'output_tokens': 20}}),
            ('message_stop', {}),
        ]
        for name, payload in events:
            self.wfile.write(f'event: {name}\ndata: {json.dumps(dict(type=name, **payload))}\n\n'.encode())
        self.wfile.flush()


class Session:
    def __init__(self, env, name, extra=()):
        self.frames = []
        self.queue = queue.Queue()
        self.name = name
        self.err = open(f'/fixture/out/{name}.stderr', 'w')
        self.process = subprocess.Popen(['/cli', '-p', '--input-format', 'stream-json',
            '--output-format', 'stream-json', '--verbose', '--replay-user-messages',
            '--setting-sources', '', '--strict-mcp-config', '--permission-prompt-tool', 'stdio',
            '--model', 'claude-haiku-4-5', *extra], env=env, stdin=subprocess.PIPE,
            stdout=subprocess.PIPE, stderr=self.err, text=True)
        self.reader = threading.Thread(target=self.read, daemon=True)
        self.reader.start()
        self.control('initialize', supportedDialogKinds=[])

    def read(self):
        for line in self.process.stdout:
            frame = json.loads(line)
            self.frames.append(frame)
            self.queue.put(frame)

    def send(self, frame):
        self.process.stdin.write(json.dumps(frame) + '\n')
        self.process.stdin.flush()

    def wait(self, pred):
        deadline = time.monotonic() + 40
        while time.monotonic() < deadline:
            frame = self.queue.get(timeout=max(0.01, deadline - time.monotonic()))
            if pred(frame):
                return frame
        raise TimeoutError(self.name)

    def control(self, subtype, **values):
        rid = str(uuid.uuid4())
        self.send({'type': 'control_request', 'request_id': rid,
                   'request': dict(subtype=subtype, **values)})
        response = self.wait(lambda f: f.get('type') == 'control_response' and f['response'].get('request_id') == rid)['response']
        assert response['subtype'] == 'success', response
        return response.get('response', {})

    def user(self, text):
        uid = str(uuid.uuid4())
        self.send({'type': 'user', 'uuid': uid, 'session_id': '', 'parent_tool_use_id': None,
                   'message': {'role': 'user', 'content': text}})
        result = self.wait(lambda f: f.get('type') == 'result' and f.get('user_message_uuid') == uid)
        assert not result.get('is_error'), result
        return uid

    def close(self):
        self.process.stdin.close()
        try:
            code = self.process.wait(timeout=15)
        except subprocess.TimeoutExpired:
            self.process.kill()
            code = self.process.wait()
        self.reader.join(timeout=5)
        self.err.close()
        Path(f'/fixture/out/{self.name}.frames.jsonl').write_text(''.join(json.dumps(f) + '\n' for f in self.frames))
        assert code == 0, code


def inside():
    env = json.loads(Path('/fixture/environment.json').read_text())
    server = http.server.ThreadingHTTPServer(('127.0.0.1', 8765), Model)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    session = Session(env, 'write')
    try:
        alpha = session.user('ALPHA 你好')
        beta = session.user('BETA discarded branch')
        gamma = session.user('GAMMA discarded branch')
        session.control('rewind_conversation', target_message_uuid=beta, last_seen_user_message_uuid=gamma)
        delta = session.user('DELTA current branch')
        session.user('/compact')
        session.user('BETWEEN_COMPACTIONS')
        session.user('/compact')
        omega = session.user('OMEGA after two compactions')
    finally:
        session.close()
    files = list(Path('/fixture/config/projects').glob('*/*.jsonl'))
    assert len(files) == 1, files
    source = files[0]
    shutil.copyfile(source, '/fixture/out/history.jsonl')
    resume = Session(env, 'resume', ['--resume', source.stem])
    try:
        exported = resume.control('export_conversation')
        request_start = len(Model.requests)
        resume.user('PROBE_RESUMED_HISTORY')
    finally:
        resume.close()
    Path('/fixture/out/resume-export.json').write_text(json.dumps(exported, indent=2))
    Path('/fixture/out/requests.json').write_text(json.dumps(Model.requests, indent=2))
    rows = [json.loads(line) for line in source.read_text().splitlines()]
    boundaries = [r for r in rows if r.get('subtype') == 'compact_boundary']
    assert len(boundaries) == 2, boundaries
    manifest = {'cli_version': subprocess.check_output(['/cli', '--version'], env=env, text=True).strip(),
                'fixture_sha256': hashlib.file_digest(open('/fixture/out/history.jsonl', 'rb'), 'sha256').hexdigest(),
                'session_id': source.stem, 'prompts': dict(alpha=alpha, beta=beta, gamma=gamma, delta=delta, omega=omega),
                'compact_boundaries': [r['uuid'] for r in boundaries],
                'resumed_model_request': request_start, 'model_requests': len(Model.requests)}
    Path('/fixture/out/manifest.json').write_text(json.dumps(manifest, indent=2))
    server.shutdown()


if __name__ == '__main__':
    if sys.argv[1:] == ['--inside']:
        inside()
    else:
        outside()

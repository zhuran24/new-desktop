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
import tempfile
import threading
import time
import uuid

CLI = Path('/mnt/wd_external/nd-build/cli/claude-2.1.289')


def outside():
    destination = Path(sys.argv[1]).resolve()
    destination.mkdir(parents=True, exist_ok=True)
    if any(destination.iterdir()):
        raise ValueError('evidence destination must be empty')
    with tempfile.TemporaryDirectory(prefix='nd-records-', dir='/mnt/wd_external/nd-build/tmp') as tmp:
        root = Path(tmp)
        for folder in ('home', 'config', 'cache', 'data', 'state', 'runtime', 'project', 'out'):
            (root / folder).mkdir()
        shutil.copyfile(__file__, root / 'generate.py')
        unit = 'nd-test-records-' + uuid.uuid4().hex[:12]
        slice_name = unit.rsplit('-', 1)[0] + unit.rsplit('-', 1)[1] + '.slice'
        subprocess.run(['busctl', '--user', 'call', 'org.freedesktop.systemd1',
            '/org/freedesktop/systemd1', 'org.freedesktop.systemd1.Manager', 'StartTransientUnit',
            'ssa(sv)a(sa(sv))', slice_name, 'fail', '3',
            'MemoryMax', 't', str(12 * 1024**3), 'MemorySwapMax', 't', '0',
            'CollectMode', 's', 'inactive-or-failed', '0'], check=True, stdout=subprocess.DEVNULL)
        try:
            group_path = subprocess.check_output(['systemctl', '--user', 'show', slice_name,
                '--property=ControlGroup', '--value'], text=True).strip()
            memory = Path('/sys/fs/cgroup') / group_path.lstrip('/')
            isolation = {'slice': slice_name, 'memory_max': (memory / 'memory.max').read_text().strip(),
                         'memory_swap_max': (memory / 'memory.swap.max').read_text().strip()}
            assert isolation['memory_max'] == str(12 * 1024**3), isolation
            assert isolation['memory_swap_max'] == '0', isolation
            (root / 'out' / 'isolation.json').write_text(json.dumps(isolation, indent=2) + '\n')
            args = ['systemd-run', '--user', '--wait', '--pipe', '--collect', '--quiet',
                    '--unit=' + unit, '--slice=' + slice_name,
                    '-p', 'MemoryMax=12G', '-p', 'MemorySwapMax=0', '-p', 'RuntimeMaxSec=180',
                    'bwrap', '--unshare-all', '--die-with-parent', '--new-session',
                    '--ro-bind', '/usr', '/usr', '--symlink', 'usr/bin', '/bin',
                    '--symlink', 'usr/lib', '/lib', '--symlink', 'usr/lib', '/lib64',
                    '--proc', '/proc', '--dev', '/dev', '--tmpfs', '/tmp', '--dir', '/etc',
                    '--bind', str(root), '/fixture', '--ro-bind', str(CLI), '/cli',
                    '--chdir', '/fixture/project', '--clearenv',
                    '--setenv', 'PATH', '/usr/bin:/bin', '--setenv', 'HOME', '/fixture/home',
                    '/usr/bin/python3', '/fixture/generate.py', '--inside']
            subprocess.run(args, check=True)
        finally:
            # Transient service is collected; this dedicated slice has no other consumers.
            subprocess.run(['systemctl', '--user', 'stop', slice_name], check=True)
            for file in (root / 'out').iterdir():
                shutil.copyfile(file, destination / file.name)
        manifest = json.loads((destination / 'manifest.json').read_text())
        manifest['cli_sha256'] = hashlib.file_digest(CLI.open('rb'), 'sha256').hexdigest()
        manifest['isolation'] = 'bwrap --unshare-all; empty environment; temporary HOME/config/XDG; unique nd-test-records*.slice; MemoryMax=12G; no host home or run mount'
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
    env = dict(PATH='/usr/bin:/bin', HOME='/fixture/home', CLAUDE_CONFIG_DIR='/fixture/config',
               XDG_CONFIG_HOME='/fixture/config', XDG_CACHE_HOME='/fixture/cache',
               XDG_DATA_HOME='/fixture/data', XDG_STATE_HOME='/fixture/state',
               XDG_RUNTIME_DIR='/fixture/runtime', LANG='C.UTF-8', TERM='dumb',
               DISABLE_AUTOUPDATER='1', DISABLE_TELEMETRY='1', DISABLE_ERROR_REPORTING='1',
               CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC='1', CLAUDE_CODE_EAGER_FLUSH='1',
               CLAUDE_CODE_SDK_READS_SESSION_STATE='1', ANTHROPIC_API_KEY='offline-fixture',
               ANTHROPIC_BASE_URL='http://127.0.0.1:8765')
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

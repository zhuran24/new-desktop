#!/usr/bin/env python3
"""Record CLI registry contracts in an isolated namespace, without owner data."""
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import threading
import time

CLI = Path('/cli')

def inside():
    spec = importlib.util.spec_from_file_location('records', '/fixture/records.py')
    records = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(records)
    env = json.loads(Path('/fixture/environment.json').read_text())
    server = records.http.server.ThreadingHTTPServer(('127.0.0.1', 8765), records.Model)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    session = records.Session(env, 'registry')
    try:
        session.user('REGISTRY_FIXTURE')
        paths = list(Path('/fixture/config/sessions').glob('*.json'))
        assert len(paths) == 1, paths
        shutil.copyfile(paths[0], '/fixture/out/session.json')
        identity = dict(pid=session.process.pid,
            start_ticks=int(Path(f'/proc/{session.process.pid}/stat').read_text().rsplit(')', 1)[1].split()[19]),
            boot_id=Path('/proc/sys/kernel/random/boot_id').read_text().strip())
        Path('/fixture/out/identity.json').write_text(json.dumps(identity, indent=2)+'\n')
        result = subprocess.run(['/cli', 'agents', '--json', '--all'], env=env, capture_output=True, check=True)
        Path('/fixture/out/agents.json').write_bytes(result.stdout)
        session_id = json.loads(paths[0].read_text())['sessionId']
        probe_env = dict(env)
        if Path('/probe').exists():
            one = subprocess.check_output(['/probe', '/fixture', str(session.process.pid), session_id, 'own'], env=probe_env, text=True)
            second = records.Session(env, 'second', ['--resume', session_id])
            try:
                duplicate = subprocess.check_output(['/probe', '/fixture', str(session.process.pid), session_id, 'duplicate'], env=probe_env, text=True)
            finally:
                second.close()
            Path('/fixture/out/probe.txt').write_text(one+duplicate)
        for path in Path('/fixture/config/projects').glob('*/*.jsonl'):
            shutil.copyfile(path, '/fixture/out/history.jsonl')
    finally:
        session.close()

    trust_path = Path('/fixture/config/.claude.json')
    trust = json.loads(trust_path.read_text()) if trust_path.exists() else {}
    trust.setdefault('projects', {}).setdefault('/fixture/project', {})['hasTrustDialogAccepted'] = True
    trust_path.write_text(json.dumps(trust))
    background = subprocess.run(['/cli', '--bg', 'BG_REGISTRY_FIXTURE', '--model', 'claude-haiku-4-5',
        '--tools', '', '--setting-sources', '', '--strict-mcp-config'], env=env, capture_output=True, text=True, timeout=30)
    Path('/fixture/out/background.stdout').write_text(background.stdout)
    Path('/fixture/out/background.stderr').write_text(background.stderr)
    Path('/fixture/out/background-status.json').write_text(json.dumps({'returncode':background.returncode}))
    time.sleep(0.5)
    assert background.returncode == 0, background.stderr
    jobs = list(Path('/fixture/config/jobs').glob('**/*'))
    assert any(p.name == 'state.json' for p in jobs), 'missing real background registry'
    Path('/fixture/out/jobs-layout.json').write_text(json.dumps([str(p.relative_to('/fixture/config')) for p in jobs], indent=2))
    for p in jobs:
        if p.is_file() and p.suffix == '.json':
            shutil.copyfile(p, '/fixture/out/job-' + p.name)
    manifest = dict(cli_version=subprocess.check_output(['/cli', '--version'], env=env, text=True).strip(),
        registry_removed_after_exit=not paths[0].exists(), network='bwrap --unshare-all', model_requests=len(records.Model.requests))
    Path('/fixture/out/manifest.json').write_text(json.dumps(manifest, indent=2)+'\n')
    server.shutdown()

def outside():
    global CLI
    sys.path.insert(0, str(Path(__file__).resolve().parents[3] / 'nd-testkit/python'))
    from isolation import Sandbox, PINNED_CLI, environment
    CLI = PINNED_CLI
    destination = Path(sys.argv[1]).resolve()
    destination.mkdir(parents=True, exist_ok=True)
    assert not any(destination.iterdir()), 'evidence destination must be empty'
    with Sandbox('claims', memory_max=12 * 1024**3, temporary_parent='/mnt/wd_external/nd-build/tmp', mount='/fixture') as box:
        root = box.root
        shutil.copyfile(__file__, root / 'generate.py')
        shutil.copyfile(Path(__file__).resolve().parents[3] / 'nd-claude-records/tests/cli/generate.py', root / 'records.py')
        (root / 'environment.json').write_text(json.dumps(environment('/fixture', claude=True, CLAUDE_CONFIG_DIR='/fixture/config')))
        (root / 'out/isolation.json').write_text(json.dumps(box.limits(), indent=2) + '\n')
        bindings = [(CLI, '/cli')]
        if len(sys.argv) > 2:
            bindings.append((Path(sys.argv[2]).resolve(), '/probe'))
        try:
            subprocess.run(box.command(['/usr/bin/python3', '/fixture/generate.py', '--inside'], bindings=bindings, runtime_max=120), check=True)
        finally:
            for file in (root / 'out').iterdir():
                shutil.copyfile(file, destination / file.name)
    manifest = json.loads((destination / 'manifest.json').read_text())
    if len(sys.argv) > 2:
        manifest['probe_sha256'] = hashlib.file_digest(Path(sys.argv[2]).open('rb'), 'sha256').hexdigest()
    manifest['repository_head'] = subprocess.check_output(['git', 'rev-parse', 'HEAD'], text=True).strip()
    manifest['cli_sha256'] = hashlib.file_digest(CLI.open('rb'), 'sha256').hexdigest()
    manifest['files'] = {p.name: hashlib.file_digest(p.open('rb'), 'sha256').hexdigest() for p in destination.iterdir() if p.name != 'manifest.json'}
    manifest['cleanup'] = {'temporary_directory_removed': not root.exists(), 'units_inactive': True, 'unit': box.unit, 'slice': box.slice}
    (destination / 'manifest.json').write_text(json.dumps(manifest, indent=2) + '\n')
    print(json.dumps(manifest, indent=2))

if __name__=='__main__':
    inside() if sys.argv[1:]==['--inside'] else outside()

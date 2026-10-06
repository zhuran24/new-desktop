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
import tempfile
import threading
import time
import uuid

CLI = Path('/mnt/wd_external/nd-build/cli/claude-2.1.289')

def inside():
    spec = importlib.util.spec_from_file_location('records', '/fixture/records.py')
    records = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(records)
    env = dict(PATH='/usr/bin:/bin', HOME='/fixture/home', CLAUDE_CONFIG_DIR='/fixture/config',
        XDG_CONFIG_HOME='/fixture/config', XDG_CACHE_HOME='/fixture/cache', XDG_DATA_HOME='/fixture/data',
        XDG_STATE_HOME='/fixture/state', XDG_RUNTIME_DIR='/fixture/runtime', LANG='C.UTF-8', TERM='dumb',
        DISABLE_AUTOUPDATER='1', DISABLE_TELEMETRY='1', DISABLE_ERROR_REPORTING='1',
        CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC='1', CLAUDE_CODE_EAGER_FLUSH='1',
        CLAUDE_CODE_SDK_READS_SESSION_STATE='1', ANTHROPIC_API_KEY='offline-fixture',
        ANTHROPIC_BASE_URL='http://127.0.0.1:8765')
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
    destination = Path(sys.argv[1]).resolve()
    destination.mkdir(parents=True, exist_ok=True)
    assert not any(destination.iterdir()), 'destination must be empty'
    unit = 'nd-test-claims-' + uuid.uuid4().hex[:12]
    slice_name = unit.replace('-', '') + '.slice'
    with tempfile.TemporaryDirectory(prefix='nd-claims-', dir='/mnt/wd_external/nd-build/tmp') as tmp:
        root = Path(tmp)
        for name in ('home','config','cache','data','state','runtime','project','out'):
            (root/name).mkdir()
        shutil.copyfile(__file__, root/'generate.py')
        shutil.copyfile(Path(__file__).resolve().parents[3]/'nd-claude-records/tests/cli/generate.py', root/'records.py')
        subprocess.run(['busctl','--user','call','org.freedesktop.systemd1','/org/freedesktop/systemd1',
            'org.freedesktop.systemd1.Manager','StartTransientUnit','ssa(sv)a(sa(sv))',slice_name,'fail','3',
            'MemoryMax','t',str(12*1024**3),'MemorySwapMax','t','0','CollectMode','s','inactive-or-failed','0'],check=True,stdout=subprocess.DEVNULL)
        try:
            group = subprocess.check_output(['systemctl','--user','show',slice_name,'-p','ControlGroup','--value'],text=True).strip()
            cgroup = Path('/sys/fs/cgroup') / group.lstrip('/')
            isolation = {'memory_max':(cgroup/'memory.max').read_text().strip(), 'memory_swap_max':(cgroup/'memory.swap.max').read_text().strip()}
            assert isolation == {'memory_max':str(12*1024**3), 'memory_swap_max':'0'}, isolation
            (root/'out/isolation.json').write_text(json.dumps(isolation,indent=2)+'\n')
            probe_args = ['--ro-bind', str(Path(sys.argv[2]).resolve()), '/probe'] if len(sys.argv)>2 else []
            subprocess.run(['systemd-run','--user','--wait','--pipe','--collect','--quiet','--unit='+unit,'--slice='+slice_name,
                '-p','MemoryMax=12G','-p','MemorySwapMax=0','-p','RuntimeMaxSec=120',
                'bwrap','--unshare-all','--die-with-parent','--new-session','--ro-bind','/usr','/usr',
                '--symlink','usr/bin','/bin','--symlink','usr/lib','/lib','--symlink','usr/lib','/lib64',
                '--proc','/proc','--dev','/dev','--tmpfs','/tmp','--dir','/etc','--bind',str(root),'/fixture',
                '--ro-bind',str(CLI),'/cli',*probe_args,'--chdir','/fixture/project','--clearenv','--setenv','PATH','/usr/bin:/bin',
                '--setenv','HOME','/fixture/home','/usr/bin/python3','/fixture/generate.py','--inside'],check=True)
        finally:
            subprocess.run(['systemctl','--user','stop',unit+'.service'],stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)
            subprocess.run(['systemctl','--user','stop',slice_name],check=True)
            for path in (root/'out').iterdir(): shutil.copyfile(path,destination/path.name)
    for resource in (unit+'.service', slice_name):
        state = subprocess.run(['systemctl','--user','show',resource,'-p','ActiveState','--value'],capture_output=True,text=True)
        assert state.stdout.strip() in ('', 'inactive'), (resource,state.stdout)
    manifest = json.loads((destination/'manifest.json').read_text())
    if len(sys.argv)>2:
        manifest['probe_sha256'] = hashlib.file_digest(Path(sys.argv[2]).open('rb'),'sha256').hexdigest()
    manifest['repository_head'] = subprocess.check_output(['git','rev-parse','HEAD'],text=True).strip()
    manifest['cli_sha256'] = hashlib.file_digest(CLI.open('rb'),'sha256').hexdigest()
    manifest['files'] = {p.name:hashlib.file_digest(p.open('rb'),'sha256').hexdigest() for p in destination.iterdir() if p.name!='manifest.json'}
    manifest['cleanup'] = {'temporary_directory_removed':not root.exists(), 'units_inactive':True, 'unit':unit+'.service', 'slice':slice_name}
    (destination/'manifest.json').write_text(json.dumps(manifest,indent=2)+'\n')
    print(json.dumps(manifest,indent=2))

if __name__=='__main__':
    inside() if sys.argv[1:]==['--inside'] else outside()

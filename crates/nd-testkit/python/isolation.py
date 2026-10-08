"""Shared real-process isolation for native scenes and CLI fixture generators."""
import json
from pathlib import Path
import shutil
import subprocess
import tempfile
import time
import uuid

POLICY = json.loads(Path(__file__).with_name('isolation.json').read_text())
PINNED_CLI = Path(POLICY['pinned_cli'])


def environment(root='/sandbox', *, claude=False, **overrides):
    values = {key: value.replace('/sandbox', root) for key, value in POLICY['environment'].items()}
    if claude:
        values.update(POLICY['offline_claude'])
    values.update(overrides)
    return values


class Sandbox:
    def __init__(self, name, *, output=None, memory_max=2 * 1024**3, temporary_parent=None, mount='/sandbox'):
        self.root = Path(tempfile.mkdtemp(prefix=f'nd-test-{name}-', dir=temporary_parent))
        self.output = Path(output).resolve() if output is not None else None
        self.memory_max = memory_max
        self.mount = mount
        identity = uuid.uuid4().hex
        self.unit = f'nd-test-{name}-{identity}.service'
        self.slice = f'nd-test-{name.replace("-", "")}{identity}.slice'
        for part in ('home', 'claude', 'config', 'data', 'state', 'cache', 'runtime', 'project', 'out'):
            (self.root / part).mkdir(mode=0o700)

    def __enter__(self):
        try:
            subprocess.run(['busctl', '--user', 'call', 'org.freedesktop.systemd1', '/org/freedesktop/systemd1',
                'org.freedesktop.systemd1.Manager', 'StartTransientUnit', 'ssa(sv)a(sa(sv))', self.slice, 'fail', '3',
                'MemoryMax', 't', str(self.memory_max), 'MemorySwapMax', 't', '0',
                'CollectMode', 's', 'inactive-or-failed', '0'], check=True, stdout=subprocess.DEVNULL)
            self.limits()
            return self
        except BaseException:
            self.__exit__(None, None, None)
            raise

    def limits(self):
        deadline = time.monotonic() + 5
        while True:
            group = subprocess.check_output(['systemctl', '--user', 'show', self.slice, '-p', 'ControlGroup', '--value'], text=True).strip()
            path = Path('/sys/fs/cgroup') / group.lstrip('/')
            if group and (path / 'memory.max').exists():
                break
            assert time.monotonic() < deadline, f'{self.slice}: transient slice did not become active'
            time.sleep(.02)
        limits = {name.replace('.', '_'): (path / name).read_text().strip() for name in ('memory.max', 'memory.swap.max')}
        assert limits == {'memory_max': str(self.memory_max), 'memory_swap_max': '0'}, limits
        return limits

    def command(self, argv, *, bindings=(), env=None, gpu=False, host_pid=False, runtime_max=300):
        args = ['systemd-run', '--user', '--quiet', '--wait', '--pipe', '--collect', '--unit', self.unit, '--slice', self.slice,
            '-p', f'MemoryMax={self.memory_max}', '-p', 'MemorySwapMax=0', '-p', 'LimitCORE=0',
            '-p', 'KillMode=control-group', '-p', 'TimeoutStopSec=3s', '-p', f'RuntimeMaxSec={runtime_max}',
            '/usr/bin/bwrap']
        args += ['--unshare-net'] if gpu else (['--unshare-user', '--unshare-ipc', '--unshare-net', '--unshare-uts'] if host_pid else ['--unshare-all'])
        args += ['--die-with-parent', '--new-session', '--ro-bind', '/usr', '/usr', '--symlink', 'usr/bin', '/bin',
            '--symlink', 'usr/lib', '/lib', '--symlink', 'usr/lib', '/lib64', '--proc', '/proc', '--dev', '/dev', '--tmpfs', '/tmp']
        args += ['--ro-bind', '/etc', '/etc', '--ro-bind', '/sys', '/sys', '--dev-bind', '/dev/dri', '/dev/dri'] if gpu else ['--dir', '/etc']
        args += ['--bind', str(self.root), self.mount]
        if self.output is not None:
            args += ['--bind', str(self.output), self.mount + '/out']
        for source, target in bindings:
            args += ['--ro-bind', str(Path(source).resolve()), str(target)]
        args += ['--chdir', self.mount + '/project', '--clearenv']
        for key, value in (env or environment(self.mount)).items():
            args += ['--setenv', key, str(value)]
        return args + list(argv)

    def native(self, script, *, bindings=(), height=900, env=None):
        (self.root / 'dbus.conf').write_text('<busconfig><type>session</type><listen>unix:tmpdir=/tmp</listen><policy context="default"><allow send_destination="*"/><allow receive_sender="*"/><allow own="*"/></policy></busconfig>')
        (self.root / 'session.sh').write_text('#!/bin/sh\nexec /usr/bin/python /scenario.py --inner\n')
        (self.root / 'session.sh').chmod(0o700)
        values = environment(XDG_CURRENT_DESKTOP='KDE', QT_QPA_PLATFORM='offscreen')
        values.update(env or {})
        return self.command(['dbus-run-session', '--config-file', '/sandbox/dbus.conf', '--', 'kwin_wayland', '--virtual',
            '--socket', 'nd-test-native', '--width', '1400', '--height', str(height), '--no-lockscreen', '--no-global-shortcuts',
            '--no-kactivities', '--exit-with-session', '/sandbox/session.sh'],
            bindings=[(script, '/scenario.py'), *bindings], env=values, gpu=True)

    def __exit__(self, *unused):
        for action, unit in [('stop', self.unit), ('stop', self.slice), ('reset-failed', self.unit)]:
            subprocess.run(['systemctl', '--user', action, unit], capture_output=True)
        shutil.rmtree(self.root)
        remaining = subprocess.check_output(['systemctl', '--user', 'list-units', self.unit, self.slice, '--no-legend', '--plain'], text=True).strip()
        cleanup = {'unit': self.unit, 'slice': self.slice, 'remaining': remaining, 'temporary_root_removed': not self.root.exists()}
        if self.output is not None:
            (self.output / 'cleanup.json').write_text(json.dumps(cleanup, indent=2))
        assert not remaining, remaining

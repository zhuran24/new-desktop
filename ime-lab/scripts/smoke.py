#!/usr/bin/env python3
"""Window smoke test in a private, headless KWin. Never sends desktop input."""
import argparse
import collections
import datetime
import hashlib
import json
import os
from pathlib import Path
import shutil
import signal
import subprocess
import sys
import tempfile
import time

ROOT = Path(__file__).resolve().parents[1]


def group_members(pgid):
    result = []
    for path in Path('/proc').glob('[0-9]*'):
        try:
            if os.getpgid(int(path.name)) == pgid:
                result.append({'pid': int(path.name), 'cmdline':
                               (path/'cmdline').read_bytes().replace(b'\0', b' ').decode(errors='replace')})
        except (ProcessLookupError, PermissionError, FileNotFoundError):
            pass
    return result


def inner():
    # This entry point is only started by the private KWin, never on the host.
    runtime = Path(os.environ['XDG_RUNTIME_DIR'])
    assert str(runtime).startswith('/tmp/ime-lab-smoke-')
    assert os.environ['WAYLAND_DISPLAY'] == os.environ['IME_LAB_SOCKET']
    assert os.environ['WAYLAND_DISPLAY'].startswith('ime-lab-')
    out = Path(os.environ['IME_LAB_SMOKE_OUTPUT'])
    variant = os.environ['IME_LAB_SMOKE_VARIANT']
    env = os.environ.copy()
    env.update(IME_LAB_SECONDS='6', IME_LAB_LOG=str(out/'events.jsonl'), WAYLAND_DEBUG='client',
               QT_QPA_PLATFORM='wayland')
    started = time.monotonic()
    with (out/'client.log').open('w') as log:
        app = subprocess.Popen([str(ROOT/'bin'/f'ime-lab-{variant}')], env=env, stdout=log, stderr=log)
        try:
            time.sleep(2)
            screenshot_code = None
            if shutil.which('spectacle') and app.poll() is None:
                with (out/'screenshot.log').open('w') as shotlog:
                    screenshot_code = subprocess.run(
                        ['spectacle', '-b', '-n', '-f', '-o', str(out/'window.png')],
                        env=env, stdout=shotlog, stderr=shotlog, timeout=10).returncode
            code = app.wait(timeout=15)
        finally:
            if app.poll() is None:
                app.terminate()
                try:
                    app.wait(timeout=3)
                except subprocess.TimeoutExpired:
                    app.kill()
                    app.wait()
    (out/'client-result.json').write_text(json.dumps({
        'exit_code': code, 'elapsed_seconds': round(time.monotonic()-started, 3),
        'screenshot_exit_code': screenshot_code, 'wayland_display': env['WAYLAND_DISPLAY'],
        'runtime_dir': str(runtime), 'pid': app.pid}, indent=2)+'\n')
    return code


def smoke(variant, guard, scenario):
    stamp = datetime.datetime.now(datetime.timezone.utc).strftime('%Y%m%dT%H%M%S%fZ')
    out = ROOT/'logs'/f'smoke-{variant}-guard{guard}-{scenario}-{stamp}'
    out.mkdir(parents=True)
    work = Path(tempfile.mkdtemp(prefix='ime-lab-smoke-', dir='/tmp'))
    env = os.environ.copy()
    for key in ['WAYLAND_DISPLAY', 'WAYLAND_SOCKET', 'DISPLAY', 'XAUTHORITY',
                'DBUS_SESSION_BUS_ADDRESS', 'DBUS_SESSION_BUS_PID', 'DBUS_STARTER_ADDRESS',
                'DBUS_STARTER_BUS_TYPE', 'DBUS_SYSTEMD_ADDRESS', 'SESSION_MANAGER',
                'QT_IM_MODULE', 'GTK_IM_MODULE', 'XMODIFIERS', 'KDE_APPLICATIONS_AS_SCOPE']:
        env.pop(key, None)
    for name, folder in [('XDG_RUNTIME_DIR', 'runtime'), ('XDG_CONFIG_HOME', 'config'),
                         ('XDG_CACHE_HOME', 'cache'), ('XDG_DATA_HOME', 'data'),
                         ('XDG_STATE_HOME', 'state')]:
        (work/folder).mkdir(mode=0o700)
        env[name] = str(work/folder)
    # No session service activation: in particular no systemd user activation
    # that could escape this private bus into the owner's desktop.
    bus_config = work/'dbus.conf'
    bus_config.write_text('''<busconfig><type>session</type>
      <listen>unix:tmpdir=/tmp</listen>
      <policy context="default"><allow send_destination="*"/>
      <allow receive_sender="*"/><allow own="*"/></policy>
    </busconfig>''')
    socket = f'ime-lab-{variant}-{os.getpid()}'
    env.update(QT_QPA_PLATFORM='offscreen', XDG_CONFIG_DIRS='/etc/xdg', IME_LAB_SOCKET=socket,
               IME_LAB_SMOKE_OUTPUT=str(out), IME_LAB_SMOKE_VARIANT=variant,
               IME_LAB_GUARD=str(guard), IME_LAB_SCENARIO=scenario)
    wrapper = work/'session.sh'
    wrapper.write_text('#!/bin/sh\nexec '+shlex_quote(sys.executable)+' '+
                       shlex_quote(str(Path(__file__).resolve()))+' --inner\n')
    wrapper.chmod(0o700)
    command = ['dbus-run-session', '--config-file', str(bus_config), '--', 'kwin_wayland',
               '--virtual', '--socket', socket, '--width', '1400', '--height', '900',
               '--no-lockscreen', '--no-global-shortcuts', '--no-kactivities',
               '--exit-with-session', str(wrapper)]
    result = {'variant': variant, 'guard': guard, 'scenario': scenario,
              'command': command, 'started_utc': stamp, 'output': str(out),
              'binary_sha256': hashlib.sha256((ROOT/'bin'/f'ime-lab-{variant}').read_bytes()).hexdigest()}
    proc = None
    try:
        with (out/'kwin.log').open('w') as log:
            proc = subprocess.Popen(command, env=env, stdout=log, stderr=log, start_new_session=True)
            result['process_group'] = proc.pid
            try:
                result['kwin_exit_code'] = proc.wait(timeout=30)
            except subprocess.TimeoutExpired:
                result['timed_out'] = True
    finally:
        if proc:
            result['processes_before_cleanup'] = group_members(proc.pid)
            if result['processes_before_cleanup']:
                os.killpg(proc.pid, signal.SIGTERM)
                for _ in range(30):
                    if not group_members(proc.pid):
                        break
                    time.sleep(0.1)
                if group_members(proc.pid):
                    os.killpg(proc.pid, signal.SIGKILL)
            proc.wait(timeout=5)
            result['processes_left'] = group_members(proc.pid)
        shutil.rmtree(work)
    client_result = out/'client-result.json'
    result['client'] = json.loads(client_result.read_text()) if client_result.exists() else None
    events_path = out/'events.jsonl'
    events = [json.loads(line) for line in events_path.read_text().splitlines()] if events_path.exists() else []
    result['events'] = dict(collections.Counter(e['event'] for e in events))
    protocol = (out/'client.log').read_text() if (out/'client.log').exists() else ''
    result['surface_buffer_attached'] = bool(__import__('re').search(r'wl_surface#\d+\.attach\(wl_buffer#', protocol))
    result['title_in_protocol'] = f'set_title("ime-lab {variant}")' in protocol
    result['screenshot_exists'] = (out/'window.png').is_file()
    times = {e['event']: e['ts_ms'] for e in events}
    result['render_alive_ms'] = times.get('timed_close', 0)-times.get('first_render', 0)
    result['pass'] = bool(result['client'] and result['client']['exit_code'] == 0
                          and result.get('kwin_exit_code') == 0 and not result['processes_left']
                          and result['events'].get('window_opened') and result['events'].get('exit')
                          and result['surface_buffer_attached'] and result['title_in_protocol']
                          and result['render_alive_ms'] >= 5000)
    if scenario != 'none':
        result['pass'] = result['pass'] and any(e['event'] == 'scenario_pass' for e in events)
    (out/'result.json').write_text(json.dumps(result, indent=2)+'\n')
    (ROOT/'evidence'/f'smoke-{variant}-guard{guard}-{scenario}.json').write_text(json.dumps(result, indent=2)+'\n')
    print(json.dumps(result, indent=2), flush=True)
    return result['pass']


def shlex_quote(text):
    import shlex
    return shlex.quote(text)


if __name__ == '__main__':
    if sys.argv[1:] == ['--inner']:
        raise SystemExit(inner())
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('variant', choices=['v070', 'main', 'all'])
    p.add_argument('--guard', type=int, choices=[0, 1], default=0)
    p.add_argument('--scenario', choices=['none', 'check'], default='none')
    args = p.parse_args()
    variants = ['v070', 'main'] if args.variant == 'all' else [args.variant]
    passed = [smoke(variant, args.guard, args.scenario) for variant in variants]
    raise SystemExit(0 if all(passed) else 1)

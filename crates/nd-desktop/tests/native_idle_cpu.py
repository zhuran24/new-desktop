#!/usr/bin/env python3
"""Release 产品经私有 KWin、真 Fcitx/Rime 的空闲 CPU 回归；不连接 owner 桌面。"""
import argparse
import contextlib
import hashlib
import json
import os
from pathlib import Path
import subprocess
import socket
import struct
import sys
import threading
import time
import uuid


def wait(predicate, seconds=15):
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        value = predicate()
        if value:
            return value
        time.sleep(.05)
    raise AssertionError('external state did not become ready')


def owner_identity():
    """Only process identity and network namespace; never reads credentials/environment."""
    network = os.readlink('/proc/self/ns/net')
    result = []
    for process in Path('/proc').glob('[0-9]*'):
        try:
            name = (process / 'comm').read_text().strip()
            if name in ('kwin_wayland', 'fcitx5') and os.readlink(process / 'ns/net') == network:
                stat = (process / 'stat').read_text().rsplit(')', 1)[1].split()
                result.append({'name': name, 'pid': int(process.name), 'start_ticks': stat[19]})
        except (FileNotFoundError, PermissionError, ProcessLookupError):
            continue
    # KWin may have CAP_SYS_NICE, which makes its namespace link unreadable to the user.
    pid = int(subprocess.check_output(['systemctl', '--user', 'show', 'plasma-kwin_wayland.service',
                                      '-p', 'MainPID', '--value'], text=True).strip() or '0')
    if pid and not any(row['pid'] == pid for row in result):
        stat = Path(f'/proc/{pid}/stat').read_text().rsplit(')', 1)[1].split()
        result.append({'name': 'kwin_wayland', 'pid': pid, 'start_ticks': stat[19]})
    return sorted(result, key=lambda row: row['pid'])


class KWin:
    def __init__(self):
        import dbus
        import dbus.service
        from dbus.mainloop.glib import DBusGMainLoop
        from gi.repository import GLib
        DBusGMainLoop(set_as_default=True)
        self.bus = dbus.SessionBus(private=True)
        self.pending = {}
        service = 'org.newdesktop.IdleCpu.p' + str(os.getpid())
        self.name = dbus.service.BusName(service, bus=self.bus)
        self.service = service

        class Reply(dbus.service.Object):
            @dbus.service.method('org.newdesktop.IdleCpu', in_signature='ss', out_signature='')
            def Snapshot(inner, token, value):
                event, result = self.pending[str(token)]
                result.append(json.loads(str(value)))
                event.set()
        self.reply = Reply(self.name, '/cpu')
        self.loop = GLib.MainLoop()
        threading.Thread(target=self.loop.run, daemon=True).start()
        self.dbus = dbus

    def query(self, operation=''):
        token = uuid.uuid4().hex
        event, result = threading.Event(), []
        self.pending[token] = event, result
        script = Path('/sandbox') / (token + '.js')
        script.write_text(operation + '''
function rect(r) { return {x:r.x,y:r.y,width:r.width,height:r.height}; }
function info(w) { return {id:String(w.internalId),pid:w.pid,inputMethod:w.inputMethod,hidden:w.hidden,
    geometry:rect(w.frameGeometry),client:rect(w.clientGeometry)}; }
var value = {active:workspace.activeWindow ? info(workspace.activeWindow) : null,
    windows:workspace.windowList().map(info)};
callDBus(SERVICE, '/cpu', 'org.newdesktop.IdleCpu', 'Snapshot', TOKEN, JSON.stringify(value));
'''.replace('SERVICE', json.dumps(self.service)).replace('TOKEN', json.dumps(token)))
        api = self.dbus.Interface(self.bus.get_object('org.kde.KWin', '/Scripting'), 'org.kde.kwin.Scripting')
        sid = api.loadScript(str(script))
        try:
            self.dbus.Interface(self.bus.get_object('org.kde.KWin', f'/Scripting/Script{sid}'), 'org.kde.kwin.Script').run()
            assert event.wait(5), 'private KWin query timed out'
            return result[0]
        finally:
            api.unloadScript(str(script))
            self.pending.pop(token)
            script.unlink()

    def activate(self, pid):
        snapshot = self.query(f'workspace.activeWindow = workspace.windowList().find(w => w.pid === {pid});\n')
        assert snapshot['active']['pid'] == pid, snapshot
        return snapshot['active']


def inner():
    assert os.environ['HOME'] == '/sandbox/home'
    assert os.environ['WAYLAND_DISPLAY'] == 'nd-test-idle-cpu'
    assert not os.environ.get('WAYLAND_SOCKET')
    assert not Path(f'/run/user/{os.getuid()}/wayland-0').exists()
    assert not Path(f'/run/user/{os.getuid()}/bus').exists()
    plan = json.loads(Path('/sandbox/plan.json').read_text())
    out = Path('/sandbox/out')
    kwin = KWin()
    import dbus
    wait(lambda: kwin.bus.name_has_owner('org.fcitx.Fcitx5'))
    controller = dbus.Interface(kwin.bus.get_object('org.fcitx.Fcitx5', '/controller'), 'org.fcitx.Fcitx.Controller1')
    handles, processes = [], []
    data = {'pass': False, 'scope': 'private KWin / real Rime / plain release',
            'configured_sample_seconds': plan['seconds'], 'acceptance_sample': plan['seconds'] >= 60, 'cpu': []}
    try:
        def start(name):
            state = Path('/sandbox/state') / (name + '.json')
            state.write_text(json.dumps({'selected_session': plan['session'], 'theme': 'dark',
                                        'window': {'width': 1050, 'height': 850}}))
            log = (out / (name + '.log')).open('w')
            handles.append(log)
            app = subprocess.Popen(['/nd-desktop', '--socket', plan['socket'], '--state', str(state)],
                                   stdout=log, stderr=log)
            processes.append(app)
            wait(lambda: next((w for w in kwin.query()['windows'] if w['pid'] == app.pid), None))
            return app
        other = None
        app = start('product')
        device_log = (out / 'input.log').open('w')
        handles.append(device_log)
        device = subprocess.Popen(['/sandbox/private-input'], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                  stderr=device_log, text=True)
        processes.append(device)

        def event(kind, code, value):
            assert kwin.query()['active']['pid'] == app.pid, 'focus changed before input'
            device.stdin.write(f'{kind} {code} {value}\n'); device.stdin.flush()
            assert device.stdout.readline() == 'ok\n', 'private input failed'

        def key(code):
            event('key', code, 1)
            event('key', code, 0)
            time.sleep(.06)

        window = kwin.activate(app.pid)
        client = window['client']
        def screenshot(name):
            path = out / (name + '.png')
            subprocess.run(['spectacle', '-b', '-n', '-f', '-o', str(path)], check=True,
                           capture_output=True, timeout=10, env=dict(os.environ, QT_QPA_PLATFORM='wayland'))
            return path
        from PIL import Image
        def editor_presented():
            with Image.open(screenshot('ready')) as image:
                return image.convert('RGB').getpixel((int(client['x'] + 330), int(client['y'] + client['height'] - 150))) != (0, 0, 0)
        wait(editor_presented)
        event('motion', client['x'] + 330, client['y'] + client['height'] - 150)
        event('button', 272, 1); event('button', 272, 0)
        event('motion', 1390, 890)
        key(29)  # Initialize the private keyboard capability without inserting text.
        time.sleep(.1)  # Fcitx receives the asynchronous text-input focus transition.
        controller.SetCurrentIM('rime')
        controller.Activate()
        wait(lambda: str(controller.CurrentInputMethod()) == 'rime')
        rime = dbus.Interface(kwin.bus.get_object('org.fcitx.Fcitx5', '/rime'), 'org.fcitx.Fcitx.Rime1')
        rime.SetSchema('luna_pinyin')
        rime.SetAsciiMode(False)
        assert not bool(rime.IsAsciiMode())

        def snapshot():
            return json.loads(subprocess.check_output(['/ndctl', '--socket', plan['socket'], 'get', 'session/' + plan['session']]))

        def draft():
            return next(i['data']['text'] for i in snapshot()['items'] if i['kind'] == 'draft')

        def candidates():
            return [w for w in kwin.query()['windows'] if w['inputMethod'] and not w['hidden']]

        def sample(name, focused, preedit=False):
            time.sleep(15)  # Include a normal idle settling interval, independently of blink policy.
            def guard():
                assert app.poll() is None
                assert kwin.query()['active']['pid'] == (app.pid if focused else other.pid)
                if preedit:
                    assert candidates(), 'Rime preedit disappeared during CPU acquisition'
            def stat():
                fields = Path(f'/proc/{app.pid}/stat').read_text().rsplit(')', 1)[1].split()
                assert fields[0] != 'Z'
                return int(fields[11]) + int(fields[12]), fields[19]
            guard()
            before, identity = stat()
            begin = time.monotonic()
            samples = []
            while time.monotonic() - begin < plan['seconds']:
                guard()
                time.sleep(min(1, max(0, plan['seconds'] - (time.monotonic() - begin))))
                ticks, current = stat()
                assert identity == current, 'CPU target PID was reused'
                samples.append({'elapsed': time.monotonic() - begin, 'ticks': ticks - before})
            guard()
            elapsed = time.monotonic() - begin
            percent = 100 * (ticks - before) / os.sysconf('SC_CLK_TCK') / elapsed
            result = {'state': name, 'pid': app.pid, 'start_ticks': identity, 'wall_seconds': elapsed,
                      'cpu_percent_one_core': percent, 'below_one_percent': percent < 1, 'samples': samples}
            data['cpu'].append(result)
            (out / 'result.json').write_text(json.dumps(data, ensure_ascii=False, indent=2))
            print(json.dumps(result), flush=True)
            if plan['seconds'] < 60:
                assert percent < 1, f'{name}: idle CPU {percent:.2f}% exceeds 1%'

        # Prove that the real editor has focus before accepting any idle sample.
        for code in [49, 23, 35, 30, 24]:
            key(code)
        wait(candidates)
        key(57)
        wait(lambda: draft() == '你好')
        event('key', 29, 1); key(30); event('key', 29, 0); key(14)
        wait(lambda: draft() == '')
        screenshot('focused-empty')
        (out / 'window.json').write_text(json.dumps(kwin.query(), indent=2))
        sample('focused-empty', True)
        for code in [49, 23, 35, 30, 24]:  # nihao, via private compositor and real Rime.
            key(code)
        try:
            wait(candidates)
        except AssertionError:
            screenshot('missing-preedit')
            raise
        assert draft() == '', 'preedit was incorrectly saved as committed text'
        screenshot('preedit')
        sample('focused-preedit', True, True)
        key(57)  # Space confirms the real Luna Pinyin candidate.
        wait(lambda: draft() == '你好')
        other = start('other')
        kwin.activate(other.pid)
        sample('unfocused', False)
        kwin.activate(app.pid)
        event('motion', client['x'] + 330, client['y'] + client['height'] - 150)
        event('button', 272, 1); event('button', 272, 0)
        event('motion', 1390, 890)
        time.sleep(.1)
        controller.SetCurrentIM('rime'); controller.Activate(); rime.SetSchema('luna_pinyin'); rime.SetAsciiMode(False)
        for code in [49, 23, 35, 30, 24]:  # Resume real input after idle and a focus transition.
            key(code)
        wait(candidates)
        key(57)
        wait(lambda: draft() == '你好你好')
        data['committed_text'] = draft()
        wait(lambda: not candidates())
        # Pixel evidence from actual compositor frames: active input still has a blinking caret.
        from PIL import ImageChops
        crops = []
        for index in range(6):
            with Image.open(screenshot(f'resumed-caret-{index}')) as image:
                crops.append(image.convert('RGB').crop((int(client['x'] + 285), int(client['y'] + client['height'] - 175),
                                                       int(client['x'] + 650), int(client['y'] + client['height'] - 120))))
            time.sleep(.17)
        differences = [ImageChops.difference(crops[0], frame).getbbox() for frame in crops[1:]]
        caret = next((rect for rect in differences if rect and 1 <= rect[2] - rect[0] <= 3 and rect[3] - rect[1] >= 8), None)
        data['active_caret_difference'] = caret
        assert caret, f'active caret did not visibly blink: {differences}'
        data['pass'] = all(row['below_one_percent'] for row in data['cpu'])
        (out / 'result.json').write_text(json.dumps(data, ensure_ascii=False, indent=2))
        assert data['pass'], 'idle product CPU must be below 1% in all three states'
    except BaseException as error:
        data['error'] = str(error)
        if 'draft' in locals():
            with contextlib.suppress(Exception):
                data['draft_at_failure'] = draft()
        if 'screenshot' in locals():
            with contextlib.suppress(Exception):
                screenshot('failure')
        with contextlib.suppress(Exception):
            (out / 'failure-window.json').write_text(json.dumps(kwin.query(), indent=2))
        (out / 'result.json').write_text(json.dumps(data, ensure_ascii=False, indent=2))
        raise
    finally:
        for process in reversed(processes):
            if process.poll() is None:
                process.terminate()
                try:
                    process.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    process.kill(); process.wait(timeout=5)
        for handle in handles:
            handle.close()
        kwin.loop.quit()


def outer(args):
    sys.path.insert(0, str(Path(__file__).resolve().parents[2] / 'nd-testkit/python'))
    from isolation import Sandbox, environment
    source = Path(__file__).parent
    # The public socket must belong to an offline test daemon, even for manual invocations.
    with socket.socket(socket.AF_UNIX) as peer:
        peer.connect(args.socket)
        pid, uid, _ = struct.unpack('3i', peer.getsockopt(socket.SOL_SOCKET, socket.SO_PEERCRED, 12))
    assert uid == os.getuid()
    assert os.readlink(f'/proc/{pid}/ns/net') != os.readlink('/proc/self/ns/net'), 'refuse a non-isolated daemon'
    assert 'nd-test-' in Path(f'/proc/{pid}/cgroup').read_text(), 'refuse a non-test daemon'
    output = Path(args.output).resolve()
    output.mkdir(parents=True, exist_ok=True)
    assert not (output / 'result.json').exists(), 'use a fresh output directory'
    before = owner_identity()
    with Sandbox('idlecpu', output=output, memory_max=3 * 1024**3,
                 temporary_parent='/mnt/wd_external/nd-build/tmp') as box:
        generated = box.root / 'input'
        generated.mkdir()
        xml = '/usr/share/plasma-wayland-protocols/fake-input.xml'
        for mode, name in [('client-header', 'fake-input.h'), ('private-code', 'fake-input.c')]:
            subprocess.run(['wayland-scanner', mode, xml, str(generated / name)], check=True)
        subprocess.run(['cc', '-Wall', '-Wextra', '-Werror', '-I', str(generated), str(source / 'private_input.c'),
                        str(generated / 'fake-input.c'), '-lwayland-client', '-lm', '-o', str(box.root / 'private-input')], check=True)
        rime = box.root / 'data/fcitx5/rime'
        rime.mkdir(parents=True)
        (rime / 'default.custom.yaml').write_text('patch:\n  schema_list:\n    - schema: luna_pinyin\n')
        with (output / 'rime-deploy.log').open('w') as log:
            subprocess.run(box.command(['rime_deployer', '--build', '/sandbox/data/fcitx5/rime',
                           '/usr/share/rime-data', '/sandbox/data/fcitx5/rime/build']), check=True, stdout=log, stderr=log)
        config = box.root / 'config/fcitx5'
        config.mkdir()
        (config / 'profile').write_text('[Groups/0]\nName=test\nDefault Layout=us\nDefaultIM=rime\n[Groups/0/Items/0]\nName=keyboard-us\n[Groups/0/Items/1]\nName=rime\nLayout=us\n[GroupOrder]\n0=test\n')
        (config / 'config').write_text('[Hotkey]\nTriggerKeys=\nAltTriggerKeys=\n[Behavior]\nActiveByDefault=True\n')
        (box.root / 'config/kwinrc').write_text('[Wayland]\nVirtualKeyboardEnabled=true\n[Plugins]\nblurEnabled=false\n[Compositing]\nAnimationSpeed=0\n')
        (box.root / 'plan.json').write_text(json.dumps({'socket': args.socket, 'session': args.session, 'seconds': args.seconds}))
        (box.root / 'dbus.conf').write_text('<busconfig><type>session</type><listen>unix:tmpdir=/tmp</listen><policy context="default"><allow send_destination="*"/><allow receive_sender="*"/><allow own="*"/></policy></busconfig>')
        (box.root / 'session.sh').write_text('#!/bin/sh\nunset WAYLAND_SOCKET\nexec python /scenario.py --inner\n')
        (box.root / 'session.sh').chmod(0o700)
        desktop = Path(args.desktop).resolve()
        with desktop.open('rb') as binary:
            (output / 'binary.json').write_text(json.dumps({'path': str(desktop), 'sha256': hashlib.file_digest(binary, 'sha256').hexdigest()}))
        bindings = [(source / 'native_idle_cpu.py', '/scenario.py'), (desktop, '/nd-desktop'),
                    (desktop.parent / 'ndctl', '/ndctl'), (Path(args.socket).parent, str(Path(args.socket).parent))]
        env = environment(XDG_CURRENT_DESKTOP='KDE', QT_QPA_PLATFORM='offscreen',
                          KWIN_WAYLAND_NO_PERMISSION_CHECKS='1', XDG_SESSION_TYPE='wayland')
        command = box.command(['dbus-run-session', '--config-file', '/sandbox/dbus.conf', '--', 'kwin_wayland',
            '--virtual', '--socket', 'nd-test-idle-cpu', '--width', '1400', '--height', '900',
            '--no-lockscreen', '--no-global-shortcuts', '--no-kactivities', '--inputmethod', 'fcitx5',
            '--exit-with-session', '/sandbox/session.sh'], bindings=bindings, env=env, gpu=True, runtime_max=400)
        with (output / 'kwin.log').open('w') as log:
            result = subprocess.run(command, stdout=log, stderr=log, timeout=410)
    after = owner_identity()
    (output / 'owner-processes.json').write_text(json.dumps({'before': before, 'after': after, 'unchanged': before == after}, indent=2))
    assert before == after, 'owner KWin/Fcitx process identity changed'
    assert result.returncode == 0, (output / 'kwin.log').read_text()[-4000:]
    data = json.loads((output / 'result.json').read_text())
    assert data['pass'], data


if __name__ == '__main__':
    if sys.argv[1:] == ['--inner']:
        inner()
    else:
        parser = argparse.ArgumentParser(description=__doc__)
        parser.add_argument('--desktop', required=True)
        parser.add_argument('--socket', required=True)
        parser.add_argument('--session', required=True)
        parser.add_argument('--output', required=True)
        parser.add_argument('--seconds', type=int, default=60)
        args = parser.parse_args()
        assert args.seconds > 0
        outer(args)

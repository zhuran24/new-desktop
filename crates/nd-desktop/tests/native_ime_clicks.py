#!/usr/bin/env python3
"""#66：私有 KWin + 真 fcitx5/Rime，经原生输入观察按钮、草稿与 nd-wire。

需要 scenarios 构建。没有输入处理函数调用，也不创建全局 uinput 设备。
"""
import argparse
import json
import os
from pathlib import Path
import subprocess
import sys
import threading
import time
import uuid


def wait(predicate, label, timeout=10):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        value = predicate()
        if value:
            return value
        time.sleep(.03)
    raise AssertionError('timed out: ' + label)


def events(path):
    result = []
    for line in path.read_text().splitlines():
        try:
            result.append(json.loads(line))
        except json.JSONDecodeError:
            pass
    return result


def owner_identity():
    result = []
    for path in Path('/proc').glob('[0-9]*'):
        try:
            name = (path / 'comm').read_text().strip()
            # KWin 带 CAP_SYS_NICE 时 /proc/environ 不可读；用公开启动参数识别真实 socket。
            owned = name == 'kwin_wayland' and b'--socket\0wayland-0\0' in (path / 'cmdline').read_bytes()
            if name == 'fcitx5':
                owned = b'XDG_RUNTIME_DIR=/run/user/' in (path / 'environ').read_bytes()
            if owned:
                result.append((int(path.name), name, (path / 'stat').read_text().rsplit(')', 1)[1].split()[19]))
        except OSError:
            pass
    return sorted(result)


def inner():
    import dbus
    import dbus.service
    from dbus.mainloop.glib import DBusGMainLoop
    from gi.repository import GLib
    from evdev import ecodes
    assert os.environ['XDG_RUNTIME_DIR'] == '/sandbox/runtime'
    assert os.environ['WAYLAND_DISPLAY'] == 'nd-test-native'
    assert os.environ['HOME'] == '/sandbox/home'
    assert not Path('/dev/uinput').exists()
    out = Path('/sandbox/out')
    plan = json.loads(Path('/sandbox/plan.json').read_text())
    DBusGMainLoop(set_as_default=True)
    bus = dbus.SessionBus()
    responses = {}
    service = 'org.newdesktop.ImeClicks.p' + str(os.getpid())
    class Reply(dbus.service.Object):
        @dbus.service.method('org.newdesktop.ImeClicks', in_signature='ss', out_signature='')
        def Snapshot(self, token, value):
            responses[str(token)] = json.loads(str(value))
    name = dbus.service.BusName(service, bus=bus)
    reply = Reply(name, '/scenario')
    loop = GLib.MainLoop()
    thread = threading.Thread(target=loop.run, daemon=True)
    thread.start()
    wait(lambda: bus.name_has_owner('org.fcitx.Fcitx5'), 'private Fcitx service')
    controller = dbus.Interface(bus.get_object('org.fcitx.Fcitx5', '/controller'), 'org.fcitx.Fcitx.Controller1')
    rime = dbus.Interface(bus.get_object('org.fcitx.Fcitx5', '/rime'), 'org.fcitx.Fcitx.Rime1')
    scripting = dbus.Interface(bus.get_object('org.kde.KWin', '/Scripting'), 'org.kde.kwin.Scripting')
    worker = subprocess.Popen(['/ime-pointer'], stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True)
    trace = (out / 'input.jsonl').open('w')
    checks = []
    app = None

    def record(value):
        trace.write(json.dumps(value, ensure_ascii=False) + '\n')
        trace.flush()

    def query():
        token = uuid.uuid4().hex
        path = Path('/sandbox') / (token + '.js')
        path.write_text('''function rect(r) { return {x:r.x,y:r.y,width:r.width,height:r.height}; }
            function info(w) { return {id:String(w.internalId),pid:w.pid,inputMethod:w.inputMethod,
                hidden:w.hidden,geometry:rect(w.frameGeometry),client:rect(w.clientGeometry)}; }
            callDBus(SERVICE, '/scenario', 'org.newdesktop.ImeClicks', 'Snapshot', TOKEN,
                JSON.stringify({active:workspace.activeWindow ? info(workspace.activeWindow):null,
                    cursor:workspace.cursorPos,windows:workspace.windowList().map(info)}));
        '''.replace('TOKEN', json.dumps(token)).replace('SERVICE', json.dumps(service)))
        sid = scripting.loadScript(str(path), timeout=3)
        try:
            dbus.Interface(bus.get_object('org.kde.KWin', f'/Scripting/Script{sid}'), 'org.kde.kwin.Script').run(timeout=3)
            return wait(lambda: responses.get(token), 'KWin snapshot', 3)
        finally:
            scripting.unloadScript(str(path), timeout=3)
            path.unlink(missing_ok=True)
            responses.pop(token, None)

    def active():
        snapshot = query()
        window = snapshot['active']
        assert app.poll() is None and window and window['pid'] == app.pid, snapshot
        return window

    def inject(kind, code, value):
        window = active()
        if kind == 'button' and value:
            cursor = query()['cursor']; rect = window['geometry']
            assert rect['x'] <= cursor['x'] < rect['x'] + rect['width']
            assert rect['y'] <= cursor['y'] < rect['y'] + rect['height']
        record(dict(kind=kind, code=code, value=value, t_ns=time.monotonic_ns(), window=window))
        worker.stdin.write(f'{kind} {code} {value}\n'); worker.stdin.flush()
        assert worker.stdout.readline().strip() == 'ok', 'private input worker exited'

    def key(name):
        code = getattr(ecodes, 'KEY_' + name)
        inject('key', code, 1)
        try:
            time.sleep(.03)
        finally:
            worker.stdin.write(f'key {code} 0\n'); worker.stdin.flush()
            assert worker.stdout.readline().strip() == 'ok'

    def view():
        return next((e['composer_view'] for e in reversed(events(stdout_path)) if 'composer_view' in e), {})

    def notice():
        return next((e['rendered_notice'] for e in reversed(events(stdout_path)) if 'rendered_notice' in e), {})

    def click(name):
        window = active()
        rows = [e['scenario_bounds']['rect'] for e in events(stdout_path)
                if e.get('scenario_bounds', {}).get('id') == name]
        assert rows, 'no laid out control: ' + name
        rect = rows[-1]
        x = window['client']['x'] + rect['x'] + rect['width'] / 2
        y = window['client']['y'] + rect['y'] + rect['height'] / 2
        frame = window['geometry']
        assert frame['x'] <= x < frame['x'] + frame['width'] and frame['y'] <= y < frame['y'] + frame['height'], name
        inject('motion', x, y)
        wait(lambda: abs(query()['cursor']['x'] - x) < 3 and abs(query()['cursor']['y'] - y) < 3, 'pointer position')
        inject('button', 272, 1)
        try:
            time.sleep(.06)
        finally:
            worker.stdin.write('button 272 0\n'); worker.stdin.flush()
            assert worker.stdout.readline().strip() == 'ok'

    def snapshot():
        value = json.loads(subprocess.check_output(['/ndctl', '--socket', plan['socket'], 'get', 'session/' + plan['session']]))
        record(dict(event='nd-wire', snapshot=value))
        return value

    def prompts():
        return [(i['id'], i['data']['text']) for i in snapshot()['items'] if i['kind'] == 'prompt']

    def candidate():
        panels = [w for w in query()['windows'] if w['inputMethod'] and not w['hidden']
                  and w['geometry']['width'] > 40 and w['geometry']['height'] > 15]
        assert len(panels) == 1, panels
        return panels[0]['geometry']

    def screenshot(label):
        subprocess.run(['spectacle', '-b', '-n', '-f', '-o', str(out / (label + '.png'))],
                       check=True, timeout=10, capture_output=True, env=dict(os.environ, QT_QPA_PLATFORM='wayland'))

    try:
        for control in ['send', 'session/' + plan['other_session'], 'new-session']:
            label = control.split('/')[0]
            stdout_path = out / (label + '.jsonl')
            state_path = Path('/sandbox/state') / (label + '.json')
            state_path.write_text(json.dumps({'selected_session': plan['session'], 'window': {'width': 1050, 'height': 850}}))
            with stdout_path.open('w') as stdout, (out / (label + '.wayland.log')).open('w') as stderr:
                app = subprocess.Popen(['/nd-desktop', '--socket', plan['socket'], '--state', str(state_path), '--quit-after', '40'],
                                       stdout=stdout, stderr=stderr, env=dict(os.environ, WAYLAND_DEBUG='client'))
                try:
                    wait(lambda: query()['active'] and query()['active']['pid'] == app.pid, 'owned active window')
                    wait(lambda: notice().get('session') == plan['session'] and view().get('bounds', {}).get('width', 0) > 0, 'native editor')
                    # Clear an earlier failing run's draft through real keyboard input.
                    controller.SetCurrentIM('keyboard-us', timeout=3); controller.Activate(timeout=3)
                    rect = view()['bounds']; window = active()
                    inject('motion', window['client']['x'] + rect['x'] + 20, window['client']['y'] + rect['y'] + 12)
                    inject('button', 272, 1); inject('button', 272, 0)
                    inject('key', ecodes.KEY_LEFTCTRL, 1); key('A'); inject('key', ecodes.KEY_LEFTCTRL, 0); key('BACKSPACE')
                    wait(lambda: view().get('text') == '', 'empty draft')
                    controller.SetCurrentIM('rime', timeout=3); controller.Activate(timeout=3)
                    rime.SetSchema('luna_pinyin', timeout=3); rime.SetAsciiMode(False, timeout=3)
                    assert str(controller.CurrentInputMethod(timeout=3)) == 'rime'
                    for char in 'nihao':
                        key(char.upper())
                    old = wait(lambda: view() if view().get('composing') and view().get('text') == 'ni hao' else None, 'real Rime preedit')
                    panel_before = candidate(); before = prompts(); screenshot(label + '-before')
                    click(control); time.sleep(.2)
                    current = view(); after = prompts(); message = notice()
                    assert after == before, dict(before=before, after=after, view=current)
                    assert message.get('session') == plan['session'] and not message.get('creating'), message
                    assert current['text'] == old['text'] and current['focused'], current
                    assert '组词' in (message.get('warning') or ''), message
                    screenshot(label + '-blocked')
                    # Completion, then a distinct click, must restore normal behavior.
                    if current['composing']:
                        key('SPACE'); wait(lambda: not view().get('composing'), 'candidate confirmation')
                    else:
                        # KWin 的 commitPendingText 已重置 Rime；重新经真输入法完成文字。
                        inject('key', ecodes.KEY_LEFTCTRL, 1); key('A'); inject('key', ecodes.KEY_LEFTCTRL, 0); key('BACKSPACE')
                        for char in 'nihao':
                            key(char.upper())
                        wait(lambda: view().get('composing'), 'new real preedit after KWin reset')
                        key('SPACE'); wait(lambda: not view().get('composing') and view().get('text') == '你好', 'candidate confirmation')
                    completed = view()['text']
                    click(control)
                    if control == 'send':
                        wait(lambda: any(text == completed for _, text in prompts()), 'completed message sent once')
                        assert len(prompts()) == len(before) + 1
                    elif control == 'new-session':
                        wait(lambda: notice().get('creating') and notice().get('session') is None, 'new session form')
                    else:
                        wait(lambda: notice().get('session') == plan['other_session'], 'selected session')
                    checks.append(dict(control=control, status='PASS', preedit=old, blocked=current,
                                       candidate_before=panel_before, completed=completed))
                except AssertionError as error:
                    screenshot(label + '-failed')
                    checks.append(dict(control=control, status='FAIL', error=str(error)))
                finally:
                    if app.poll() is None:
                        app.terminate()
                    app.wait(timeout=5)
        verdict = dict(pass_=all(c['status'] == 'PASS' for c in checks), checks=checks, desktop_injection=False)
        verdict['pass'] = verdict.pop('pass_')
        (out / 'result.json').write_text(json.dumps(verdict, ensure_ascii=False, indent=2))
        assert verdict['pass'], verdict
    finally:
        if app is not None and app.poll() is None:
            app.terminate(); app.wait(timeout=5)
        worker.stdin.close(); worker.wait(timeout=5)
        trace.close()
        reply.remove_from_connection()
        loop.quit(); thread.join(timeout=3)


def outer(args):
    sys.path.insert(0, str(Path(__file__).resolve().parents[2] / 'nd-testkit/python'))
    from isolation import Sandbox, environment
    before = owner_identity()
    out = args.output.resolve(); out.mkdir(parents=True, exist_ok=False)
    helper_dir = args.desktop.resolve().parent / 'ime-clicks'
    helper_dir.mkdir(exist_ok=True)
    protocol = '/usr/share/plasma-wayland-protocols/fake-input.xml'
    for mode, name in [('client-header', 'fake-input.h'), ('private-code', 'fake-input.c')]:
        subprocess.run(['wayland-scanner', mode, protocol, str(helper_dir / name)], check=True)
    source = Path(__file__).with_name('ime_pointer.c')
    flags = subprocess.check_output(['pkg-config', '--cflags', '--libs', 'wayland-client'], text=True).split()
    helper = helper_dir / 'ime-pointer'
    subprocess.run(['cc', '-O2', '-Wall', '-Wextra', '-Werror', '-I' + str(helper_dir), str(source),
                    str(helper_dir / 'fake-input.c'), '-o', str(helper), *flags], check=True)
    with Sandbox('imeclicks', output=out, memory_max=3 * 1024**3,
                 temporary_parent='/mnt/wd_external/nd-build/tmp') as box:
        cfg = box.root / 'config/fcitx5'; cfg.mkdir()
        (cfg / 'profile').write_text('[Groups/0]\nName=test\nDefault Layout=us\nDefaultIM=rime\n'
                                   '[Groups/0/Items/0]\nName=keyboard-us\n[Groups/0/Items/1]\nName=rime\nLayout=us\n[GroupOrder]\n0=test\n')
        (cfg / 'config').write_text('[Hotkey]\nTriggerKeys=\nAltTriggerKeys=\n[Behavior]\nActiveByDefault=True\nShareInputState=No\n')
        rime = box.root / 'data/fcitx5/rime'; rime.mkdir(parents=True)
        (rime / 'default.custom.yaml').write_text('patch:\n  schema_list:\n    - schema: luna_pinyin\n')
        with (out / 'rime-deploy.log').open('w') as log:
            subprocess.run(box.command(['rime_deployer', '--build', '/sandbox/data/fcitx5/rime', '/usr/share/rime-data',
                                        '/sandbox/data/fcitx5/rime/build']), check=True, stdout=log, stderr=log, timeout=120)
        (box.root / 'config/kwinrc').write_text('[Wayland]\nVirtualKeyboardEnabled=true\n')
        (box.root / 'dbus.conf').write_text('<busconfig><type>session</type><listen>unix:tmpdir=/tmp</listen>'
            '<policy context="default"><allow send_destination="*"/><allow receive_sender="*"/><allow own="*"/></policy></busconfig>')
        (box.root / 'fcitx.sh').write_text('#!/bin/sh\nexec fcitx5 -D --disable all --enable wayland,waylandim,keyboard,rime,classicui,dbus,dbusfrontend > /sandbox/out/fcitx.log 2>&1\n')
        (box.root / 'fcitx.sh').chmod(0o700)
        (box.root / 'session.sh').write_text('#!/bin/sh\nunset WAYLAND_SOCKET\nexec python /scenario.py --inner\n')
        (box.root / 'session.sh').chmod(0o700)
        (box.root / 'plan.json').write_text(json.dumps(dict(socket=str(args.socket), session=args.session, other_session=args.other_session)))
        bindings = [(Path(__file__), '/scenario.py'), (args.desktop, '/nd-desktop'),
                    (args.desktop.with_name('ndctl'), '/ndctl'), (helper, '/ime-pointer'), (args.socket.parent, args.socket.parent)]
        command = box.command(['dbus-run-session', '--config-file', '/sandbox/dbus.conf', '--', 'kwin_wayland',
            '--virtual', '--socket', 'nd-test-native', '--width', '1400', '--height', '1000', '--no-lockscreen',
            '--no-global-shortcuts', '--no-kactivities', '--inputmethod', '/sandbox/fcitx.sh',
            '--exit-with-session', '/sandbox/session.sh'], bindings=bindings,
            env=environment(XDG_CURRENT_DESKTOP='KDE', QT_QPA_PLATFORM='offscreen', KWIN_WAYLAND_NO_PERMISSION_CHECKS='1',
                            QT_LOGGING_RULES='kwin_*.debug=true'), gpu=True, runtime_max=150)
        with (out / 'kwin.log').open('w') as log:
            result = subprocess.run(command, stdout=log, stderr=log, timeout=165)
    after = owner_identity()
    (out / 'owner-processes.json').write_text(json.dumps(dict(before=before, after=after, unchanged=before == after), indent=2))
    assert before == after
    assert result.returncode == 0, (out / 'kwin.log').read_text()[-5000:]
    print((out / 'result.json').read_text())


if __name__ == '__main__':
    if sys.argv[1:] == ['--inner']:
        inner()
    else:
        parser = argparse.ArgumentParser(description=__doc__)
        parser.add_argument('--desktop', type=Path, required=True)
        parser.add_argument('--socket', type=Path, required=True)
        parser.add_argument('--session', required=True)
        parser.add_argument('--other-session', required=True)
        parser.add_argument('--output', type=Path, required=True)
        outer(parser.parse_args())

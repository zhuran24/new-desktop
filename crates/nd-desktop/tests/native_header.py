#!/usr/bin/env python3
"""真守护进程、私有 KWin/fcitx5/Rime：会话头保持可见可点，对话单独滚动。"""
import argparse
import json
import os
from pathlib import Path
import select
import subprocess
import sys
import threading
import time
import uuid


def inner():
    import dbus
    import dbus.service
    from dbus.mainloop.glib import DBusGMainLoop
    from gi.repository import GLib

    assert os.environ['WAYLAND_DISPLAY'] == 'nd-test-header'
    assert os.environ['XDG_RUNTIME_DIR'] == '/sandbox/runtime'
    assert '/run/user/' not in os.environ['DBUS_SESSION_BUS_ADDRESS']
    plan = json.loads(Path('/sandbox/plan.json').read_text())
    out = Path('/sandbox/out')
    Path('/sandbox/state/ui.json').write_text(json.dumps({'selected_session': plan['session']}))
    DBusGMainLoop(set_as_default=True)
    bus = dbus.SessionBus(private=True)
    pending = {}
    service = 'org.newdesktop.NativeHeader'

    class Reply(dbus.service.Object):
        @dbus.service.method('org.newdesktop.NativeHeader', in_signature='ss', out_signature='')
        def Snapshot(self, token, data):
            if str(token) in pending:
                pending[str(token)][1].append(json.loads(str(data)))
                pending[str(token)][0].set()

    name = dbus.service.BusName(service, bus=bus)
    reply = Reply(name, '/test')
    loop = GLib.MainLoop()
    thread = threading.Thread(target=loop.run, daemon=True)
    thread.start()
    app = worker = None
    checks = []
    geometry = {}

    def query(width=None, height=None):
        token = uuid.uuid4().hex
        event = threading.Event()
        values = []
        pending[token] = (event, values)
        path = Path('/sandbox') / (token + '.js')
        mutation = '' if width is None else f'w.frameGeometry = {{x:30,y:30,width:{width},height:{height}}}; workspace.activeWindow = w;'
        path.write_text('''
            var w = workspace.windowList().find(w => w.pid === PID && !w.inputMethod);
            function rect(r) { return {x:r.x,y:r.y,width:r.width,height:r.height}; }
            if (w) { MUTATION }
            callDBus('org.newdesktop.NativeHeader','/test','org.newdesktop.NativeHeader','Snapshot',TOKEN,
                JSON.stringify({active:workspace.activeWindow ? workspace.activeWindow.pid : null,
                    client:w ? rect(w.clientGeometry) : null, frame:w ? rect(w.frameGeometry) : null,
                    cursor:{x:workspace.cursorPos.x,y:workspace.cursorPos.y}}));
        '''.replace('PID', str(app.pid)).replace('MUTATION', mutation).replace('TOKEN', json.dumps(token)))
        api = dbus.Interface(bus.get_object('org.kde.KWin', '/Scripting'), 'org.kde.kwin.Scripting')
        sid = api.loadScript(str(path), timeout=3)
        try:
            dbus.Interface(bus.get_object('org.kde.KWin', f'/Scripting/Script{sid}'), 'org.kde.kwin.Script').run(timeout=3)
            assert event.wait(3), 'private KWin did not answer'
            return values[0]
        finally:
            api.unloadScript(str(path), timeout=3)
            path.unlink()
            pending.pop(token)

    def logs():
        values = []
        for line in (out / 'desktop.jsonl').read_text().splitlines():
            try:
                values.append(json.loads(line))
            except json.JSONDecodeError:
                pass
        return values

    def latest(key):
        return next((v[key] for v in reversed(logs()) if key in v), {})

    def wait(predicate, reason, timeout=10):
        until = time.monotonic() + timeout
        while time.monotonic() < until:
            assert app.poll() is None, (out / 'desktop.log').read_text()[-3000:]
            value = predicate()
            if value:
                return value
            time.sleep(.03)
        raise AssertionError(reason + ': ' + (out / 'desktop.jsonl').read_text()[-2500:])

    def bounds(control):
        return next((v['native_layout'] for v in reversed(logs())
                     if v.get('native_layout', {}).get('id') == control), None)

    def visible(control):
        r = bounds(control)
        assert r and r['width'] > 0 and r['height'] > 0, f'{control}: no painted bounds'
        assert r['x'] >= 0 and r['y'] >= 0 and r['x'] + r['width'] <= r['viewport'][0] + 1 and r['y'] + r['height'] <= r['viewport'][1] + 1, f'{control} outside viewport: {r}'
        return r

    def send(kind, a, b):
        q = query()
        assert q['active'] == app.pid, 'input focus left the private product window'
        if kind == 'button' or kind == 'wheel':
            r, p = q['client'], q['cursor']
            assert r['x'] <= p['x'] < r['x'] + r['width'] and r['y'] <= p['y'] < r['y'] + r['height']
        worker.stdin.write(f'{kind} {a} {b}\n')
        worker.stdin.flush()
        assert select.select([worker.stdout], [], [], 3)[0], 'private input helper timed out'
        assert worker.stdout.readline().strip() == 'ok', 'private input helper stopped'

    def click(x, y):
        send('motion', x, y)
        wait(lambda: abs(query()['cursor']['x'] - x) < 2 and abs(query()['cursor']['y'] - y) < 2, 'pointer did not move')
        send('button', 272, 1)
        send('button', 272, 0)

    def control_click(control):
        r = visible(control)
        c = query()['client']
        click(c['x'] + r['x'] + r['width']/2, c['y'] + r['y'] + r['height']/2)

    def key(code):
        send('key', code, 1)
        send('key', code, 0)

    def compose_chinese():
        r = visible('composer')
        c = query()['client']
        click(c['x'] + r['x'] + 50, c['y'] + r['y'] + r['height'] - 120)
        fcitx = dbus.Interface(bus.get_object('org.fcitx.Fcitx5', '/controller'), 'org.fcitx.Fcitx.Controller1')
        time.sleep(.1)
        wait(lambda: 'program:new-desktop' in str(fcitx.DebugInfo()) and 'focus:1' in str(fcitx.DebugInfo()), 'Fcitx did not focus the product editor')
        fcitx.SetCurrentIM('rime'); fcitx.Activate()
        (out / 'fcitx-focus.txt').write_text(str(fcitx.DebugInfo()))
        # Rime 的 D-Bus 对象在插件第一次被激活后才登记。
        rime = dbus.Interface(bus.get_object('org.fcitx.Fcitx5', '/rime'), 'org.fcitx.Fcitx.Rime1')
        rime.SetSchema('luna_pinyin'); rime.SetAsciiMode(False)
        for code in [49, 23, 35, 30, 24]:  # nihao, Linux key codes
            key(code)
        wait(lambda: latest('rendered_editor').get('composing'), 'real Rime preedit did not reach product')
        key(57)
        wait(lambda: not latest('rendered_editor').get('composing') and '你好' in latest('rendered_editor').get('text', ''), 'Rime did not commit Chinese')
        return str(fcitx.CurrentInputMethod())

    def screenshot(label):
        from PIL import Image
        path = out / (label + '.png')
        for _ in range(10):
            subprocess.run(['spectacle', '-b', '-n', '-f', '-o', str(path)], check=True,
                           capture_output=True, timeout=10, env=dict(os.environ, QT_QPA_PLATFORM='wayland'))
            with Image.open(path) as image:
                if sum(image.convert('L').histogram()[16:]) > image.width * image.height * .05:
                    return
            time.sleep(.1)
        raise AssertionError('compositor did not present a product frame')

    try:
        with (out / 'desktop.jsonl').open('w') as stdout, (out / 'desktop.log').open('w') as stderr:
            app = subprocess.Popen(['/nd-desktop', '--socket', plan['socket'], '--state', '/sandbox/state/ui.json',
                '--scenario-controls', '/sandbox/no-actions.json', '--quit-after', '90'], stdout=stdout, stderr=stderr)
            wait(lambda: latest('rendered_session').get('items'), 'session did not load')
            wait(lambda: query()['client'], 'product window not found')
            api = dbus.Interface(bus.get_object('org.kde.KWin', '/org/kde/KWin/EIS/RemoteDesktop'), 'org.kde.KWin.EIS.RemoteDesktop')
            fd, cookie = api.connectToEIS(dbus.Int32(2))
            fd = fd.take()
            try:
                worker = subprocess.Popen(['/native-input', str(fd)], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                          text=True, pass_fds=(fd,))
            finally:
                os.close(fd)
            for width, height in [(1050, 850), (800, 600)]:
                query(width, height)
                geometry = wait(lambda: (q if abs(q['frame']['width'] - width) <= 2 and abs(q['frame']['height'] - height) <= 2 else None)
                                if (q := query())['frame'] else None, 'window size not acknowledged')
                wait(lambda: all(bounds(control) and bounds(control)['viewport'] == [geometry['client']['width'], geometry['client']['height']]
                                 for control in ['settings', 'session-header', 'composer', 'conversation']), 'layout did not follow resize')
                input_method = compose_chinese()
                screenshot(f'{width}x{height}')
                header = visible('session-header')
                settings = visible('settings')
                control_click('settings')
                wait(lambda: latest('native_controls').get('settings_open') is True, 'visible settings button did not open settings')
                visible('session-header'); visible('settings')
                screenshot(f'{width}x{height}-settings')
                control_click('settings')
                wait(lambda: latest('native_controls').get('settings_open') is False, 'settings did not close')
                wait(lambda: bounds('conversation') and bounds('conversation')['height'] > 0, 'conversation did not return after closing settings')
                content = visible('conversation')
                before = bounds('session-header')
                messages_before = bounds('messages')
                c = query()['client']
                send('motion', c['x'] + content['x'] + 40, c['y'] + content['y'] + content['height']/2)
                send('wheel', 0, 2400)
                wait(lambda: bounds('messages')['y'] != messages_before['y'], 'conversation did not scroll')
                assert bounds('session-header') == before, 'conversation scroll moved the fixed session header'
                visible('session-header'); control_click('settings')
                wait(lambda: latest('native_controls').get('settings_open') is True, 'header lost clickability after conversation scroll')
                control_click('settings')
                wait(lambda: latest('native_controls').get('settings_open') is False, 'settings did not close after scroll')
                screenshot(f'{width}x{height}-scrolled')
                checks.append({'size': [width, height], 'geometry': geometry, 'header': header, 'settings': settings,
                               'header_after_scroll': bounds('session-header'), 'editor': latest('rendered_editor')})
            (out / 'result.json').write_text(json.dumps({'pass': True, 'checks': checks, 'input_method': input_method,
                                                        'desktop_injection': False}, ensure_ascii=False, indent=2))
    except BaseException as error:
        (out / 'result.json').write_text(json.dumps({'pass': False, 'error': str(error), 'checks': checks,
                                                   'geometry': geometry, 'editor': latest('rendered_editor'), 'desktop_injection': False}, ensure_ascii=False, indent=2))
        raise
    finally:
        for process in [worker, app]:
            if process is not None and process.poll() is None:
                process.terminate()
                process.wait(timeout=5)
        reply.remove_from_connection()
        loop.quit(); thread.join(timeout=2)


def run(args):
    sys.path.insert(0, str(Path(__file__).resolve().parents[2] / 'nd-testkit/python'))
    from isolation import Sandbox, environment
    out = Path(args.output).resolve()
    out.mkdir(parents=True, exist_ok=True)
    with Sandbox('header', output=out, temporary_parent='/mnt/wd_external/nd-build/tmp') as box:
        helper = out / 'helper'
        helper.mkdir(exist_ok=True)
        xml = '/usr/share/plasma-wayland-protocols/fake-input.xml'
        for mode, file in [('client-header', 'fake-input.h'), ('private-code', 'fake-input.c')]:
            subprocess.run(['wayland-scanner', mode, xml, str(helper / file)], check=True)
        flags = subprocess.check_output(['pkg-config', '--cflags', '--libs', 'wayland-client', 'libei-1.0'], text=True).split()
        subprocess.run(['cc', '-Wall', '-Wextra', '-Werror', '-I' + str(helper), str(Path(__file__).with_name('native_input.c')),
                        str(helper / 'fake-input.c'), '-o', str(helper / 'native-input'), *flags], check=True)
        root = box.root
        (root / 'plan.json').write_text(json.dumps({'socket': args.socket, 'session': args.session}))
        rime = root / 'data/fcitx5/rime'
        rime.mkdir(parents=True)
        (rime / 'default.custom.yaml').write_text('patch:\n  schema_list:\n    - schema: luna_pinyin\n')
        with (out / 'rime-deploy.log').open('w') as log:
            subprocess.run(box.command(['rime_deployer', '--build', '/sandbox/data/fcitx5/rime', '/usr/share/rime-data',
                                        '/sandbox/data/fcitx5/rime/build']), stdout=log, stderr=log, check=True, timeout=120)
        config = root / 'config/fcitx5'
        config.mkdir()
        (config / 'profile').write_text('[Groups/0]\nName=test\nDefault Layout=us\nDefaultIM=rime\n[Groups/0/Items/0]\nName=keyboard-us\n[Groups/0/Items/1]\nName=rime\nLayout=us\n[GroupOrder]\n0=test\n')
        (config / 'config').write_text('[Hotkey]\nTriggerKeys=\nAltTriggerKeys=\n[Behavior]\nActiveByDefault=True\nShareInputState=No\n')
        (root / 'config/kwinrc').write_text('[Wayland]\nVirtualKeyboardEnabled=true\n[Plugins]\nblurEnabled=false\n')
        (root / 'dbus.conf').write_text('<busconfig><type>session</type><listen>unix:tmpdir=/tmp</listen><policy context="default"><allow send_destination="*"/><allow receive_sender="*"/><allow own="*"/></policy></busconfig>')
        (root / 'session.sh').write_text('#!/bin/sh\nexec python /scenario.py --inner\n')
        (root / 'session.sh').chmod(0o700)
        desktop = Path(args.desktop).resolve()
        command = box.command(['dbus-run-session', '--config-file', '/sandbox/dbus.conf', '--', 'kwin_wayland', '--virtual',
            '--socket', 'nd-test-header', '--width', '1400', '--height', '1100', '--no-lockscreen', '--no-global-shortcuts',
            '--no-kactivities', '--inputmethod', 'fcitx5', '--exit-with-session', '/sandbox/session.sh'],
            bindings=[(Path(__file__), '/scenario.py'), (desktop, '/nd-desktop'), (Path(args.socket).parent, str(Path(args.socket).parent)),
                      (helper / 'native-input', '/native-input')],
            env=environment(XDG_CURRENT_DESKTOP='KDE', QT_QPA_PLATFORM='offscreen', KWIN_WAYLAND_NO_PERMISSION_CHECKS='1', XDG_SESSION_TYPE='wayland'), gpu=True)
        with (out / 'kwin.log').open('w') as log:
            result = subprocess.run(command, stdout=log, stderr=log, timeout=100)
        assert result.returncode == 0, (out / 'kwin.log').read_text()[-5000:]
        assert json.loads((out / 'result.json').read_text())['pass']
    print(f'PASS native session header: {out}')


if __name__ == '__main__':
    if sys.argv[1:] == ['--inner']:
        inner()
    else:
        parser = argparse.ArgumentParser(description=__doc__)
        for key in ['desktop', 'socket', 'session', 'output']:
            parser.add_argument('--' + key, required=True)
        run(parser.parse_args())

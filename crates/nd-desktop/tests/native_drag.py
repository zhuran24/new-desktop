#!/usr/bin/env python3
"""私有 KWin/fcitx5/Rime 中，用真实 Qt 拖放把文件加入产品草稿。"""
import argparse
import json
import os
from pathlib import Path
import select
import re
import subprocess
import sys
import threading
import time
import uuid


def source(file):
    from PySide6.QtCore import QMimeData, Qt, QUrl
    from PySide6.QtGui import QDrag
    from PySide6.QtWidgets import QApplication, QLabel

    class Source(QLabel):
        def mousePressEvent(self, event):
            self.origin = event.position().toPoint()

        def mouseMoveEvent(self, event):
            if event.buttons() & Qt.LeftButton and (event.position().toPoint() - self.origin).manhattanLength() > 8:
                mime = QMimeData()
                mime.setUrls([QUrl.fromLocalFile(str(Path(file).resolve()))])
                print(json.dumps({'offered_uri_list': bytes(mime.data('text/uri-list')).decode()}), flush=True)
                drag = QDrag(self)
                drag.setMimeData(mime)
                print(json.dumps({'drag_started': str(file)}), flush=True)
                print(json.dumps({'drag_result': int(drag.exec(Qt.CopyAction).value)}), flush=True)

    app = QApplication(sys.argv)
    app.setDesktopFileName('nd-test-drag-source')
    label = Source(Path(file).name)
    label.resize(320, 180)
    label.show()
    raise SystemExit(app.exec())


def inner():
    import dbus
    import dbus.service
    from dbus.mainloop.glib import DBusGMainLoop
    from gi.repository import GLib

    # native_input.c also refuses every other socket and inherited Wayland fd.
    assert os.environ['WAYLAND_DISPLAY'] == 'nd-test-header'
    assert os.environ['XDG_RUNTIME_DIR'] == '/sandbox/runtime'
    assert '/run/user/' not in os.environ['DBUS_SESSION_BUS_ADDRESS']
    assert 'WAYLAND_SOCKET' not in os.environ
    plan = json.loads(Path('/sandbox/plan.json').read_text())
    out = Path('/sandbox/out')
    Path('/sandbox/state/ui.json').write_text(json.dumps({'selected_session': plan['session']}))
    DBusGMainLoop(set_as_default=True)
    bus = dbus.SessionBus(private=True)
    pending = {}
    service = 'org.newdesktop.NativeDrag'

    class Reply(dbus.service.Object):
        @dbus.service.method(service, in_signature='ss', out_signature='')
        def Snapshot(self, token, data):
            if str(token) in pending:
                pending[str(token)][1].append(json.loads(str(data)))
                pending[str(token)][0].set()

    name = dbus.service.BusName(service, bus=bus)
    reply = Reply(name, '/test')
    loop = GLib.MainLoop()
    thread = threading.Thread(target=loop.run, daemon=True)
    thread.start()
    app = worker = drag = None
    checks = []

    def query(pid=None, geometry=None):
        token = uuid.uuid4().hex
        event, values = threading.Event(), []
        pending[token] = event, values
        path = Path('/sandbox') / (token + '.js')
        mutation = '' if geometry is None else 'w.frameGeometry = ' + json.dumps(geometry) + '; workspace.activeWindow = w;'
        path.write_text('''
            var w = workspace.windowList().find(w => w.pid === PID && !w.inputMethod);
            function rect(r) { return {x:r.x,y:r.y,width:r.width,height:r.height}; }
            if (w) { MUTATION }
            callDBus('org.newdesktop.NativeDrag','/test','org.newdesktop.NativeDrag','Snapshot',TOKEN,
                JSON.stringify({active:workspace.activeWindow ? workspace.activeWindow.pid : null,
                    client:w ? rect(w.clientGeometry) : null,
                    cursor:{x:workspace.cursorPos.x,y:workspace.cursorPos.y}}));
        '''.replace('PID', str(pid or app.pid)).replace('MUTATION', mutation).replace('TOKEN', json.dumps(token)))
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

    def rows(file):
        values = []
        for line in (out / file).read_text().splitlines():
            try:
                values.append(json.loads(line))
            except json.JSONDecodeError:
                pass
        return values

    def latest(key):
        result = subprocess.run(['/ndctl', '--socket', plan['socket'], 'get', 'session/' + plan['session']],
                                capture_output=True, text=True, check=True, timeout=5)
        snapshot = json.loads(result.stdout)
        if key == 'rendered_session':
            return snapshot
        draft = next(i['data'] for i in snapshot['items'] if i['kind'] == 'draft')
        preedit = re.findall(r'preedit_string\((nil|".*?")', (out / 'desktop.log').read_text())
        return dict(text=draft['text'], attachments=draft['attachments'],
                    composing=bool(preedit and preedit[-1] not in ('""', 'nil')))

    def wait(predicate, reason, timeout=8):
        until = time.monotonic() + timeout
        while time.monotonic() < until:
            assert app.poll() is None, 'product exited'
            value = predicate()
            if value:
                return value
            time.sleep(.03)
        raise AssertionError(reason + ': ' + json.dumps(latest('rendered_editor'), ensure_ascii=False))

    def bounds():
        # 本场景固定为 1050×850。输入框在会话窗底部；按合成器实际客户区换算。
        c = query()['client']
        return c['x'] + 310, c['y'] + c['height'] - 155

    def send(kind, a, b):
        assert query()['active'] in {app.pid, drag.pid if drag else app.pid}, 'input focus left the private test windows'
        worker.stdin.write(f'{kind} {a} {b}\n')
        worker.stdin.flush()
        assert select.select([worker.stdout], [], [], 3)[0], 'private input helper timed out'
        assert worker.stdout.readline().strip() == 'ok', 'private input helper stopped'

    def key(code):
        send('key', code, 1)
        send('key', code, 0)

    def screenshot(label):
        from PIL import Image
        path = out / (label + '.png')
        subprocess.run(['spectacle', '-b', '-n', '-f', '-o', str(path)],
                       check=True, capture_output=True, timeout=10, env=dict(os.environ, QT_QPA_PLATFORM='wayland'))
        with Image.open(path) as image:
            assert sum(image.convert('L').histogram()[16:]) > image.width * image.height * .05, 'compositor screenshot is empty'

    try:
        with (out / 'desktop.jsonl').open('w') as stdout, (out / 'desktop.log').open('w') as stderr:
            app = subprocess.Popen(['/nd-desktop', '--socket', plan['socket'], '--state', '/sandbox/state/ui.json',
                '--quit-after', '75'], stdout=stdout, stderr=stderr, env=dict(os.environ, WAYLAND_DEBUG='client'))
            wait(lambda: latest('rendered_session').get('items'), 'session did not load')
            wait(lambda: query()['client'], 'product window not found')
            query(geometry=dict(x=30, y=30, width=1050, height=850))
            wait(lambda: (c := query()['client']) and c['width'] == 1050 and c['height'] == 814,
                 'product resize was not acknowledged')
            screenshot('ready')
            api = dbus.Interface(bus.get_object('org.kde.KWin', '/org/kde/KWin/EIS/RemoteDesktop'), 'org.kde.KWin.EIS.RemoteDesktop')
            fd, _cookie = api.connectToEIS(dbus.Int32(2))
            fd = fd.take()
            try:
                worker = subprocess.Popen(['/native-input', str(fd)], stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True, pass_fds=(fd,))
            finally:
                os.close(fd)
            x, y = bounds()
            send('motion', x, y); send('button', 272, 1); send('button', 272, 0)
            time.sleep(.1)
            fcitx = dbus.Interface(bus.get_object('org.fcitx.Fcitx5', '/controller'), 'org.fcitx.Fcitx.Controller1')
            wait(lambda: 'program:new-desktop' in str(fcitx.DebugInfo()) and 'focus:1' in str(fcitx.DebugInfo()), 'Fcitx did not focus product')
            fcitx.SetCurrentIM('rime'); fcitx.Activate()
            rime = dbus.Interface(bus.get_object('org.fcitx.Fcitx5', '/rime'), 'org.fcitx.Fcitx.Rime1')
            rime.SetSchema('luna_pinyin'); rime.SetAsciiMode(False)
            (out / 'fcitx-before.txt').write_text(str(fcitx.DebugInfo()))
            for code in [49, 23, 35, 30, 24]:
                key(code)
            wait(lambda: latest('rendered_editor').get('composing'), 'real Rime preedit did not reach product')
            key(57)
            wait(lambda: latest('rendered_editor').get('text') == '你好', 'Rime did not commit Chinese')
            (out / 'fcitx-focus.txt').write_text(str(fcitx.DebugInfo()))
            # Ctrl+A / Backspace 到达产品键盘路径；Rime 的组词键本身由输入法消费。
            send('key', 29, 1); key(30); send('key', 29, 0); key(14)
            wait(lambda: latest('rendered_editor')['text'] == '', 'draft was not cleared')
            from PIL import Image
            text_file = Path('/sandbox/project/拖入 中文 和空格.txt')
            text_file.write_text('真实 Wayland 拖放正文')
            image_file = Path('/sandbox/project/拖入 图片 和空格.png')
            Image.new('RGB', (16, 16), '#2386d1').save(image_file)
            cancelled_file = Path('/sandbox/project/取消 拖入.txt')
            cancelled_file.write_text('取消的文件不能成为附件')
            for index, file in enumerate([text_file, image_file, cancelled_file]):
                with (out / f'source-{index}.jsonl').open('w') as source_out, (out / f'source-{index}.log').open('w') as source_err:
                    drag = subprocess.Popen(['python', '/scenario.py', '--source', str(file)], stdout=source_out, stderr=source_err,
                                            env=dict(os.environ, QT_QPA_PLATFORM='wayland', WAYLAND_DEBUG='client'))
                    wait(lambda: query(drag.pid)['client'], 'Qt drag source not found')
                    query(drag.pid, dict(x=750, y=120, width=320, height=216))
                    c = wait(lambda: query(drag.pid)['client'], 'source geometry missing')
                    start = c['x'] + 100, c['y'] + 80
                    dest = bounds() if index < 2 else (1300, 1000)
                    send('motion', *start); send('button', 272, 1)
                    try:
                        for n in range(1, 31):
                            send('motion', start[0] + (dest[0] - start[0]) * n / 30,
                                 start[1] + (dest[1] - start[1]) * n / 30)
                            time.sleep(.02)
                    finally:
                        send('button', 272, 0)
                    wait(lambda: any(v.get('drag_result') == (1 if index < 2 else 0) for v in rows(f'source-{index}.jsonl')), 'Qt drag did not finish with the expected action')
                    screenshot(f'dropped-{index}')
                    if index < 2:
                        wait(lambda: any(a['name'] == file.name for a in latest('rendered_editor').get('attachments', [])), 'dropped file did not become an attachment')
                    assert latest('rendered_editor')['text'] == '', 'dropping a file changed the draft text'
                    checks.append({'file': file.name, 'source': rows(f'source-{index}.jsonl'), 'editor': latest('rendered_editor')})
                drag.terminate(); drag.wait(timeout=5); drag = None
                if index == 2:
                    query(geometry=dict(x=30, y=30, width=1050, height=850))
                    x, y = bounds()
                    send('motion', x, y); send('button', 272, 1); send('button', 272, 0)
                    # 离开窗口的拖放不能残留到下一次普通点击；小文件上传用公开草稿核对。
                    for _ in range(10):
                        assert len(latest('rendered_editor')['attachments']) == 2, 'cancelled drag became an attachment'
                        time.sleep(.05)
            (out / 'result.json').write_text(json.dumps({'pass': True, 'checks': checks, 'desktop_injection': False}, ensure_ascii=False, indent=2))
    except BaseException as error:
        (out / 'result.json').write_text(json.dumps({'pass': False, 'error': str(error), 'checks': checks,
            'editor': latest('rendered_editor'), 'desktop_injection': False}, ensure_ascii=False, indent=2))
        raise
    finally:
        for process in [worker, drag, app]:
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
    with Sandbox('drag', output=out, temporary_parent='/mnt/wd_external/nd-build/tmp') as box:
        helper = out / 'helper'
        helper.mkdir(exist_ok=True)
        for mode, file in [('client-header', 'fake-input.h'), ('private-code', 'fake-input.c')]:
            subprocess.run(['wayland-scanner', mode, '/usr/share/plasma-wayland-protocols/fake-input.xml', str(helper / file)], check=True)
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
        command = box.command(['dbus-run-session', '--config-file', '/sandbox/dbus.conf', '--', 'kwin_wayland', '--virtual',
            '--socket', 'nd-test-header', '--width', '1400', '--height', '1100', '--no-lockscreen', '--no-global-shortcuts',
            '--no-kactivities', '--inputmethod', 'fcitx5', '--exit-with-session', '/sandbox/session.sh'],
            bindings=[(Path(__file__), '/scenario.py'), (Path(args.desktop), '/nd-desktop'),
                      (Path(args.socket).parent, str(Path(args.socket).parent)), (helper / 'native-input', '/native-input'), (Path(args.ndctl), '/ndctl')],
            env=environment(XDG_CURRENT_DESKTOP='KDE', QT_QPA_PLATFORM='offscreen', KWIN_WAYLAND_NO_PERMISSION_CHECKS='1', XDG_SESSION_TYPE='wayland'), gpu=True)
        with (out / 'kwin.log').open('w') as log:
            result = subprocess.run(command, stdout=log, stderr=log, timeout=100)
        assert result.returncode == 0, (out / 'kwin.log').read_text()[-5000:]
        assert json.loads((out / 'result.json').read_text())['pass']
    print(f'PASS native Wayland attachment drag: {out}')


if __name__ == '__main__':
    if sys.argv[1:] == ['--inner']:
        inner()
    elif len(sys.argv) == 3 and sys.argv[1] == '--source':
        source(sys.argv[2])
    else:
        parser = argparse.ArgumentParser(description=__doc__)
        for key in ['desktop', 'ndctl', 'socket', 'session', 'output']:
            parser.add_argument('--' + key, required=True)
        run(parser.parse_args())

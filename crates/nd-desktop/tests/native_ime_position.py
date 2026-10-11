#!/usr/bin/env python3
"""#73：私有双输出 KWin + 真 Rime，候选位置由同尺寸截图中的字形独立判定。"""
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

from native_ime_clicks import events, owner_identity, wait


def inner():
    import dbus
    import dbus.service
    from dbus.mainloop.glib import DBusGMainLoop
    from gi.repository import GLib
    from evdev import ecodes
    from PIL import Image, ImageDraw

    assert os.environ['XDG_RUNTIME_DIR'] == '/sandbox/runtime'
    assert os.environ['WAYLAND_DISPLAY'] == 'nd-test-native'
    assert os.environ['HOME'] == '/sandbox/home'
    assert not Path('/dev/uinput').exists()
    out = Path('/sandbox/out')
    plan = json.loads(Path('/sandbox/plan.json').read_text())
    DBusGMainLoop(set_as_default=True)
    bus = dbus.SessionBus()
    responses = {}
    service = 'org.newdesktop.ImePosition.p' + str(os.getpid())

    class Reply(dbus.service.Object):
        @dbus.service.method('org.newdesktop.ImePosition', in_signature='ss', out_signature='')
        def Snapshot(self, token, value):
            responses[str(token)] = json.loads(str(value))

    name = dbus.service.BusName(service, bus=bus)
    reply = Reply(name, '/scenario')
    loop = GLib.MainLoop()
    thread = threading.Thread(target=loop.run, daemon=True)
    thread.start()
    wait(lambda: bus.name_has_owner('org.fcitx.Fcitx5'), 'private Fcitx service')
    controller = dbus.Interface(bus.get_object('org.fcitx.Fcitx5', '/controller'), 'org.fcitx.Fcitx.Controller1')
    scripting = dbus.Interface(bus.get_object('org.kde.KWin', '/Scripting'), 'org.kde.kwin.Scripting')
    worker = subprocess.Popen(['/ime-pointer'], stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True)
    trace = (out / 'input.jsonl').open('w')
    checks = []
    app = None
    stdout_path = out / 'desktop.jsonl'

    def query(mutation=''):
        token = uuid.uuid4().hex
        path = Path('/sandbox') / (token + '.js')
        path.write_text("""function rect(r) { return {x:r.x,y:r.y,width:r.width,height:r.height}; }
            function info(w) { return {pid:w.pid,inputMethod:w.inputMethod,hidden:w.hidden,
                geometry:rect(w.frameGeometry),client:rect(w.clientGeometry),output:w.output.name}; }
            var w=workspace.windowList().find(w=>w.pid===PID && !w.inputMethod);
            if(w){ MUTATION }
            callDBus(SERVICE, '/scenario', 'org.newdesktop.ImePosition', 'Snapshot', TOKEN,
                JSON.stringify({active:workspace.activeWindow ? info(workspace.activeWindow):null,
                    windows:workspace.windowList().map(info),cursor:workspace.cursorPos,
                    outputs:workspace.screens.map(o=>({name:o.name,geometry:rect(o.geometry)}))}));
        """.replace('PID', str(app.pid)).replace('MUTATION', mutation)
              .replace('TOKEN', json.dumps(token)).replace('SERVICE', json.dumps(service)))
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
        trace.write(json.dumps(dict(kind=kind,code=code,value=value,window=window,t_ns=time.monotonic_ns()))+'\n')
        trace.flush()
        worker.stdin.write(f'{kind} {code} {value}\n'); worker.stdin.flush()
        assert select.select([worker.stdout], [], [], 3)[0], 'private input timeout'
        assert worker.stdout.readline().strip() == 'ok', 'private input worker exited'

    def key(name):
        code = getattr(ecodes, 'KEY_' + name)
        inject('key', code, 1)
        try:
            time.sleep(.03)
        finally:
            inject('key', code, 0)

    def view():
        return next((e['composer_view'] for e in reversed(events(stdout_path)) if 'composer_view' in e), {})

    def editor():
        return next((e['native_layout'] for e in reversed(events(stdout_path))
                     if e.get('native_layout', {}).get('id') == 'composer'), {})

    def candidate():
        panels = [w for w in query()['windows'] if w['inputMethod'] and not w['hidden']
                  and w['geometry']['width'] > 40 and w['geometry']['height'] > 15]
        assert len(panels) == 1, panels
        return panels[0]['geometry']

    def screenshot(label):
        path = out / (label + '.png')
        subprocess.run(['spectacle','-b','-n','-f','-o',str(path)], check=True, timeout=10,
                       capture_output=True, env=dict(os.environ, QT_QPA_PLATFORM='wayland'))
        return Image.open(path).convert('RGB')

    def compose(plain=''):
        for char in 'nihao':
            key(char.upper())
        wait(lambda: view().get('composing') and view().get('text') == plain + 'ni hao', 'real Rime preedit')
        wait(lambda: candidate(), 'visible Rime candidates')
        time.sleep(.15)

    def measure(label, plain=''):
        assert view().get('composing'), 'preedit lost during window operation'
        # A screenshot may itself flush pending KWin/Fcitx frames. Accept geometry only
        # when it stays unchanged across that capture, so stale panel bounds cannot
        # leave the real panel's pixels in the independent glyph search.
        for attempt in range(6):
            window = active()
            panel = candidate()
            bounds = view()['bounds']  # Whole textarea layout, never IME/caret geometry.
            before = screenshot(label + '-preedit')
            if window == active() and panel == candidate() and bounds == view()['bounds']:
                break
        else:
            raise AssertionError('geometry did not settle across screenshot: ' + label)
        assert bounds and bounds['height'] > 0
        key('ESC')
        wait(lambda: not view().get('composing') and view().get('text') == plain, 'cancel real preedit')
        time.sleep(.15)
        after = screenshot(label + '-plain')
        assert active()['client'] == window['client'], 'reference changed client geometry'
        assert before.size == after.size, 'reference changed screenshot size'
        left = round(window['client']['x'] + bounds['x'])
        top = round(window['client']['y'] + bounds['y'])
        right = left + round(bounds['width'])
        bottom = top + round(bounds['height'])
        points = []
        for y in range(top, bottom):
            for x in range(left, right):
                if panel['x'] <= x < panel['x']+panel['width'] and panel['y'] <= y < panel['y']+panel['height']:
                    continue
                a,b = before.getpixel((x,y)),after.getpixel((x,y))
                # Neutral text pixels exclude the colored blinking caret.
                if max(a)-min(a) < 35 and max(a)>95 and max(abs(a[i]-b[i]) for i in range(3))>35:
                    points.append((x,y))
        assert len(points)>25, f'no independent preedit glyphs: {label}'
        glyph = dict(x=min(p[0] for p in points), y=min(p[1] for p in points),
                     right=max(p[0] for p in points)+1, bottom=max(p[1] for p in points)+1)
        assert 20 <= glyph['right']-glyph['x'] <= 100 and 5 <= glyph['bottom']-glyph['y'] <= 28, glyph
        marked = before.copy()
        draw = ImageDraw.Draw(marked)
        draw.rectangle((glyph['x'],glyph['y'],glyph['right'],glyph['bottom']),outline='lime',width=2)
        draw.rectangle((panel['x'],panel['y'],panel['x']+panel['width'],panel['y']+panel['height']),outline='red',width=2)
        marked.save(out / (label + '-measured.png'))
        result = dict(label=label,window=window,panel=panel,glyph=glyph,
                      gap_y=panel['y']-glyph['bottom'],gap_x=panel['x']-glyph['x'])
        checks.append(result)
        (out / 'measurements.json').write_text(json.dumps(checks,indent=2))
        # Default private ClassicUI is below the caret. A one-line stale offset must fail.
        assert -3 <= result['gap_y'] <= 18 and abs(result['gap_x']) <= 14, result
        return result

    try:
        state_path = Path('/sandbox/state/ui.json')
        state_path.write_text(json.dumps({'selected_session':plan['session'],'window':{'width':1050,'height':850}}))
        with stdout_path.open('w') as stdout, (out / 'desktop.wayland.log').open('w') as stderr:
            app = subprocess.Popen(['/nd-desktop','--socket',plan['socket'],'--state',str(state_path),'--quit-after','80'],
                                   stdout=stdout,stderr=stderr,env=dict(os.environ,WAYLAND_DEBUG='client'))
            wait(lambda: query()['active'] and query()['active']['pid']==app.pid, 'owned active window')
            query('w.frameGeometry={x:50,y:50,width:1050,height:850};')
            wait(lambda: editor().get('height',0)>0 and view().get('bounds',{}).get('height',0)>0, 'painted editor')
            time.sleep(.2)
            window=active(); rect=view()['bounds']
            inject('motion',window['client']['x']+rect['x']+20,window['client']['y']+rect['y']+12)
            inject('button',272,1); inject('button',272,0)
            controller.SetCurrentIM('rime',timeout=3); controller.Activate(timeout=3)
            rime = dbus.Interface(bus.get_object('org.fcitx.Fcitx5', '/rime'), 'org.fcitx.Fcitx.Rime1')
            rime.SetSchema('luna_pinyin',timeout=3); rime.SetAsciiMode(False,timeout=3)
            assert str(controller.CurrentInputMethod(timeout=3))=='rime'
            compose(); measure('baseline')
            compose()
            old=active()['client']
            query('w.frameGeometry={x:50,y:50,width:970,height:810};')
            wait(lambda: active()['client']['height']<old['height'], 'window shrank')
            time.sleep(.25)
            measure('shrink')
            if plan['only'] == 'all':
                compose()
                query('w.frameGeometry={x:50,y:50,width:1050,height:850};')
                wait(lambda: active()['client']['height']>old['height']-1, 'window enlarged')
                time.sleep(.25)
                measure('enlarge')
                outputs = query()['outputs']
                assert len(outputs) == 2, outputs
                for destination in [outputs[1], outputs[0]]:
                    compose()
                    frame=active()['geometry']
                    start=(frame['x']+frame['width']/2,frame['y']+8)
                    target=destination['geometry']
                    end=(target['x']+target['width']/2,target['y']+80)
                    inject('motion',*start); inject('button',272,1)
                    try:
                        for n in range(1,21):
                            inject('motion',start[0]+(end[0]-start[0])*n/20,
                                   start[1]+(end[1]-start[1])*n/20)
                            time.sleep(.01)
                    finally:
                        inject('button',272,0)
                    wait(lambda: active()['output']==destination['name'], 'window crossed output')
                    time.sleep(.25)
                    measure('drag-to-'+destination['name'])
                # Reflow a scrolled multi-line draft with Chinese and supplementary-plane characters.
                prefix='中文🙂'*70+'\n'+'右侧正文'*8
                subprocess.run(['wl-copy',prefix],check=True,timeout=5)
                inject('key',ecodes.KEY_LEFTCTRL,1)
                try: key('V')
                finally: inject('key',ecodes.KEY_LEFTCTRL,0)
                wait(lambda: view().get('text')==prefix, 'native clipboard draft')
                compose(prefix)
                query('w.frameGeometry={x:50,y:50,width:770,height:850};')
                wait(lambda: active()['client']['width']==770, 'wrapped window width')
                time.sleep(.25)
                measure('wrapped-reflow',prefix)
                inject('key',ecodes.KEY_LEFTCTRL,1)
                try: key('A')
                finally: inject('key',ecodes.KEY_LEFTCTRL,0)
                key('BACKSPACE')
                wait(lambda: view().get('text')=='', 'clear draft through keyboard')
            (out / 'result.json').write_text(json.dumps({'pass':True,'checks':checks},indent=2))
    finally:
        if app is not None and app.poll() is None:
            app.kill(); app.wait(timeout=5)
        worker.stdin.close(); worker.wait(timeout=5)
        trace.close()
        loop.quit(); thread.join(timeout=3)


def outer(args):
    sys.path.insert(0, str(Path(__file__).resolve().parents[2] / 'nd-testkit/python'))
    from isolation import Sandbox, environment
    before = owner_identity()
    out = args.output.resolve(); out.mkdir(parents=True, exist_ok=False)
    helper_dir = args.desktop.resolve().parent / 'ime-position'
    helper_dir.mkdir(exist_ok=True)
    protocol = '/usr/share/plasma-wayland-protocols/fake-input.xml'
    for mode, name in [('client-header', 'fake-input.h'), ('private-code', 'fake-input.c')]:
        subprocess.run(['wayland-scanner', mode, protocol, str(helper_dir / name)], check=True)
    source = Path(__file__).with_name('ime_pointer.c')
    flags = subprocess.check_output(['pkg-config', '--cflags', '--libs', 'wayland-client', 'libei-1.0'], text=True).split()
    helper = helper_dir / 'ime-pointer'
    subprocess.run(['cc', '-O2', '-Wall', '-Wextra', '-Werror', '-I' + str(helper_dir), str(source),
                    str(helper_dir / 'fake-input.c'), '-o', str(helper), *flags], check=True)
    with Sandbox('imeposition', output=out, memory_max=3 * 1024**3,
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
        (box.root / 'plan.json').write_text(json.dumps(dict(socket=str(args.socket), session=args.session, only=args.only)))
        bindings = [(Path(__file__), '/scenario.py'), (Path(__file__).with_name('native_ime_clicks.py'), '/native_ime_clicks.py'), (args.desktop, '/nd-desktop'),
                    (args.desktop.with_name('ndctl'), '/ndctl'), (helper, '/ime-pointer'), (args.socket.parent, args.socket.parent)]
        command = box.command(['dbus-run-session', '--config-file', '/sandbox/dbus.conf', '--', 'kwin_wayland',
            '--virtual', '--output-count', '2', '--socket', 'nd-test-native', '--width', '1400', '--height', '1000', '--no-lockscreen',
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
        parser.add_argument('--only', choices=['shrink', 'all'], default='all')
        parser.add_argument('--output', type=Path, required=True)
        outer(parser.parse_args())

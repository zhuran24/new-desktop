#!/usr/bin/env python3
"""#67：私有 KWin、真实 Fcitx/Rime，按住 Esc 不应成为新按键。"""
import argparse
import json
import os
from pathlib import Path
import subprocess
import sys
import threading
import time


def inner():
    assert os.environ['WAYLAND_DISPLAY'] == 'nd-test-ime'
    assert os.environ['XDG_RUNTIME_DIR'] == '/sandbox/runtime'
    assert 'WAYLAND_SOCKET' not in os.environ
    import dbus
    import dbus.service
    from dbus.mainloop.glib import DBusGMainLoop
    from gi.repository import GLib
    DBusGMainLoop(set_as_default=True)
    bus = dbus.SessionBus()
    out = Path('/sandbox/out')
    values, wake = [], threading.Event()

    class Reply(dbus.service.Object):
        @dbus.service.method('org.newdesktop.EscapeTest', in_signature='s', out_signature='')
        def Focus(self, value):
            values.append(json.loads(str(value))); wake.set()

    name = dbus.service.BusName('org.newdesktop.EscapeTest', bus=bus)
    reply = Reply(name, '/test')
    loop = GLib.MainLoop()
    thread = threading.Thread(target=loop.run, daemon=True); thread.start()
    scripting = dbus.Interface(bus.get_object('org.kde.KWin', '/Scripting'), 'org.kde.kwin.Scripting')
    app = keyboard = None
    result = {'pass': False, 'checks': []}
    handles = []

    def focus():
        wake.clear(); values.clear()
        path = Path('/sandbox/focus.js')
        path.write_text('var w=workspace.activeWindow; callDBus("org.newdesktop.EscapeTest", "/test",'
                        '"org.newdesktop.EscapeTest", "Focus", JSON.stringify({pid:w?w.pid:null}));')
        sid = scripting.loadScript(str(path))
        try:
            dbus.Interface(bus.get_object('org.kde.KWin', f'/Scripting/Script{sid}'), 'org.kde.kwin.Script').run()
            assert wake.wait(3), 'private KWin did not report focus'
            assert values[-1]['pid'] == app.pid, values[-1]
        finally:
            scripting.unloadScript(str(path))

    def state():
        reports = []
        for line in (out/'lab.jsonl').read_text().splitlines():
            try:
                value = json.loads(line)
                if 'composer_lab_state' in value: reports.append(value['composer_lab_state'])
            except json.JSONDecodeError: pass
        return reports[-1] if reports else {}

    def wait(predicate, seconds=5):
        end = time.monotonic()+seconds
        while time.monotonic()<end:
            assert app.poll() is None, 'test window exited'
            value = state()
            if predicate(value): return value
            time.sleep(.02)
        raise AssertionError(state())

    def event(key, value):
        if value: focus()
        keyboard.stdin.write(f'{key} {value}\n'); keyboard.stdin.flush()
        assert keyboard.stdout.readline().strip() == f'{key} {value}', 'private keyboard stopped'
        inputs.write(json.dumps({'key':key,'value':value,'t_ns':time.monotonic_ns()})+'\n'); inputs.flush()

    def key(code, hold=.03):
        event(code, 1)
        try:
            end = time.monotonic()+hold
            while time.monotonic()<end:
                time.sleep(min(.02, max(0, end-time.monotonic()))); focus()
        finally: event(code, 0)

    try:
        stdout = (out/'lab.jsonl').open('w'); stderr = (out/'lab.wayland.log').open('w')
        inputs = (out/'input.jsonl').open('w'); handles += [stdout,stderr,inputs]
        app = subprocess.Popen(['/lab','--quit-after','40'],stdout=stdout,stderr=stderr,
                               env=dict(os.environ,WAYLAND_DEBUG='client'))
        wait(lambda s:'escapes' in s)
        end=time.monotonic()+5
        while True:
            try:
                focus(); break
            except AssertionError:
                if time.monotonic()>=end: raise
                time.sleep(.05)
        eis = dbus.Interface(bus.get_object('org.kde.KWin','/org/kde/KWin/EIS/RemoteDesktop'),
                             'org.kde.KWin.EIS.RemoteDesktop')
        descriptor, _ = eis.connectToEIS(dbus.Int32(1)); descriptor=descriptor.take()
        try:
            keyboard=subprocess.Popen(['/keyboard',str(descriptor)],stdin=subprocess.PIPE,
                                      stdout=subprocess.PIPE,text=True,pass_fds=(descriptor,))
        finally: os.close(descriptor)
        assert keyboard.stdout.readline().strip()=='ready', 'private keyboard not ready'
        controller = dbus.Interface(bus.get_object('org.fcitx.Fcitx5','/controller'), 'org.fcitx.Fcitx.Controller1')
        # Populate a genuine clipboard draft before entering Rime composition.
        controller.SetCurrentIM('keyboard-us'); controller.Activate()
        subprocess.run(['wl-copy','左🙂右'],check=True,timeout=5)
        event(29,1)
        try: key(47)
        finally: event(29,0)
        wait(lambda s:s['text']=='左🙂右'); key(105)
        controller.SetCurrentIM('rime'); controller.Activate()
        rime = dbus.Interface(bus.get_object('org.fcitx.Fcitx5','/rime'), 'org.fcitx.Fcitx.Rime1')
        end = time.monotonic()+5
        while True:
            rime.SetSchema('luna_pinyin'); rime.SetAsciiMode(False)
            if not rime.IsAsciiMode(): break
            assert time.monotonic()<end, 'Rime context not ready'; time.sleep(.05)
        time.sleep(.15)
        for code in [49,23,35,30,24]: key(code)
        wait(lambda s:s['composing'] and s['text'].startswith('左🙂') and s['text'].endswith('右'))
        before = state()['escapes']
        key(1,.9); time.sleep(.1)
        after = wait(lambda s:not s['composing'])
        result['before'], result['after'] = before, after
        assert after['text']=='左🙂右' and after['escapes']==before, after
        result['checks'].append('取消组词后按住 Esc 不增加非组词计数，正文保留')
        key(1); wait(lambda s:s['escapes']==before+1)
        result['pass'] = True
        result['checks'].append('松开后再按一次 Esc 只增加一次')
    finally:
        if keyboard is not None:
            keyboard.stdin.close(); keyboard.wait(timeout=5)
        if app is not None and app.poll() is None: app.terminate(); app.wait(timeout=5)
        result['final'] = state()
        (out/'result.json').write_text(json.dumps(result,ensure_ascii=False,indent=2))
        for handle in handles: handle.close()
        reply.remove_from_connection(); loop.quit(); thread.join(timeout=2)


def run(args):
    sys.path.insert(0,str(Path(__file__).resolve().parents[2]/'nd-testkit/python'))
    from isolation import Sandbox, environment
    out = Path(args.output).resolve(); out.mkdir(parents=True,exist_ok=True)
    for old in ['result.json','cleanup.json']: (out/old).unlink(missing_ok=True)
    helper = Path(args.bin_dir).resolve().parent/'helpers/private-keyboard'
    helper.parent.mkdir(parents=True,exist_ok=True)
    flags = subprocess.check_output(['pkg-config','--cflags','--libs','libei-1.0'],text=True).split()
    subprocess.run(['cc','-O2','-Wall','-Wextra','-Werror',str(Path(__file__).with_name('private_keyboard.c')),'-o',str(helper),*flags],check=True)
    with Sandbox('escape67',output=out,memory_max=3*1024**3,
                 temporary_parent='/mnt/wd_external/nd-build/tmp') as box:
        rime=box.root/'data/fcitx5/rime'; rime.mkdir(parents=True)
        (rime/'default.custom.yaml').write_text('patch:\n  schema_list:\n    - schema: luna_pinyin\n')
        with (out/'rime-deploy.log').open('w') as log:
            subprocess.run(box.command(['rime_deployer','--build','/sandbox/data/fcitx5/rime','/usr/share/rime-data','/sandbox/data/fcitx5/rime/build']),stdout=log,stderr=log,check=True,timeout=120)
        cfg=box.root/'config/fcitx5'; cfg.mkdir()
        (cfg/'profile').write_text('[Groups/0]\nName=test\nDefault Layout=us\nDefaultIM=rime\n[Groups/0/Items/0]\nName=keyboard-us\n[Groups/0/Items/1]\nName=rime\nLayout=us\n[GroupOrder]\n0=test\n')
        (box.root/'config/kwinrc').write_text('[Wayland]\nVirtualKeyboardEnabled=true\n')
        (box.root/'dbus.conf').write_text('<busconfig><type>session</type><listen>unix:tmpdir=/tmp</listen><policy context="default"><allow send_destination="*"/><allow receive_sender="*"/><allow own="*"/></policy></busconfig>')
        (box.root/'session.sh').write_text('#!/bin/sh\nunset WAYLAND_SOCKET\nexec python /test.py --inner\n'); (box.root/'session.sh').chmod(0o700)
        command=box.command(['dbus-run-session','--config-file','/sandbox/dbus.conf','--','kwin_wayland','--virtual','--socket','nd-test-ime','--width','1400','--height','900','--no-lockscreen','--no-global-shortcuts','--no-kactivities','--inputmethod','fcitx5','--exit-with-session','/sandbox/session.sh'],gpu=True,
            bindings=[(Path(__file__),'/test.py'),(Path(args.bin_dir)/'nd-composer-lab','/lab'),(helper,'/keyboard')],
            env=environment(XDG_CURRENT_DESKTOP='KDE',QT_QPA_PLATFORM='offscreen',KWIN_WAYLAND_NO_PERMISSION_CHECKS='1',XDG_SESSION_TYPE='wayland'),runtime_max=60)
        with (out/'kwin.log').open('w') as log: completed=subprocess.run(command,stdout=log,stderr=log,timeout=75)
    print((out/'result.json').read_text())
    assert completed.returncode==0, (out/'kwin.log').read_text()[-4000:]


if __name__=='__main__':
    if sys.argv[1:]==['--inner']: inner()
    else:
        parser=argparse.ArgumentParser(description=__doc__)
        parser.add_argument('--bin-dir',required=True); parser.add_argument('--output',required=True)
        run(parser.parse_args())

#!/usr/bin/env python3
"""#67：私有 KWin、真实 Fcitx/Rime，按住 Esc 不应成为新按键。"""
import argparse
import json
import os
import re
import select
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
    plan = json.loads(Path('/sandbox/plan.json').read_text())
    product = plan['product']
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
                        '"org.newdesktop.EscapeTest", "Focus", JSON.stringify({pid:w?w.pid:null,client:w?{x:w.clientGeometry.x,y:w.clientGeometry.y}:null,cursor:{x:workspace.cursorPos.x,y:workspace.cursorPos.y}}));')
        sid = scripting.loadScript(str(path))
        try:
            dbus.Interface(bus.get_object('org.kde.KWin', f'/Scripting/Script{sid}'), 'org.kde.kwin.Script').run()
            assert wake.wait(3), 'private KWin did not report focus'
            assert values[-1]['pid'] == app.pid, values[-1]
            return values[-1]
        finally:
            scripting.unloadScript(str(path))

    def reports():
        values = []
        for line in (out/'lab.jsonl').read_text().splitlines():
            try: values.append(json.loads(line))
            except json.JSONDecodeError: pass
        return values

    def state():
        values = reports()
        if not product:
            return next((v['composer_lab_state'] for v in reversed(values) if 'composer_lab_state' in v), {})
        controls = next((v['native_controls'] for v in reversed(values) if 'native_controls' in v), {})
        session = next((v['rendered_session'] for v in reversed(values) if 'rendered_session' in v), {})
        header = next((v['data'] for v in session.get('items', []) if v['kind']=='header'), {})
        return dict(controls, running=header.get('process', {}).get('turn_running'),
                    can_rewind=header.get('interaction', {}).get('rewind_menu'))

    def unchanged(start, running, panel):
        for value in reports()[start:]:
            if 'native_controls' in value:
                assert value['native_controls']['panel'] == panel, value
            if 'rendered_session' in value:
                for item in value['rendered_session']['items']:
                    if item['kind'] == 'header':
                        assert item['data']['process']['turn_running'] == running, item
        assert state()['running'] == running and state()['panel'] == panel, state()

    def pointer(command):
        focus()
        keyboard.stdin.write(command+'\n'); keyboard.stdin.flush()
        assert select.select([keyboard.stdout], [], [], 3)[0], 'pointer timed out'
        assert keyboard.stdout.readline().strip() == 'ok'

    def focus_composer():
        bounds = next(v['native_layout'] for v in reversed(reports())
                      if v.get('native_layout', {}).get('id') == 'composer')
        client = focus()['client']
        x = client['x']+bounds['x']+50
        y = client['y']+bounds['y']+bounds['height']-120
        pointer(f'motion {x} {y}')
        wait(lambda _:abs(focus()['cursor']['x']-x)<2 and abs(focus()['cursor']['y']-y)<2)
        wait(lambda _:re.search(r'wl_pointer#\d+\.enter\(', (out/'lab.wayland.log').read_text()))
        pointer('button 1')
        try: wait(lambda s:s.get('focused'))
        finally: pointer('button 0')

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
        assert select.select([keyboard.stdout], [], [], 3)[0], 'private keyboard timed out'
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
        if product:
            Path('/sandbox/state/ui.json').write_text(json.dumps({'selected_session':plan['session']}))
        command = ['/nd-desktop','--socket',plan['socket'],'--state','/sandbox/state/ui.json',
                   '--scenario-controls','/sandbox/no-actions.json','--quit-after','70'] if product else ['/lab','--quit-after','70']
        app = subprocess.Popen(command,stdout=stdout,stderr=stderr,
                               env=dict(os.environ,WAYLAND_DEBUG='client'))
        wait(lambda s:s.get('running') is True if product else 'escapes' in s)
        end=time.monotonic()+5
        while True:
            try:
                focus(); break
            except AssertionError:
                if time.monotonic()>=end: raise
                time.sleep(.05)
        eis = dbus.Interface(bus.get_object('org.kde.KWin','/org/kde/KWin/EIS/RemoteDesktop'),
                             'org.kde.KWin.EIS.RemoteDesktop')
        descriptor, _ = eis.connectToEIS(dbus.Int32(3 if product else 1)); descriptor=descriptor.take()
        try:
            keyboard=subprocess.Popen(['/keyboard',str(descriptor),*(['--pointer'] if product else [])],stdin=subprocess.PIPE,
                                      stdout=subprocess.PIPE,text=True,pass_fds=(descriptor,))
        finally: os.close(descriptor)
        assert keyboard.stdout.readline().strip()=='ready', 'private keyboard not ready'
        if product: focus_composer()
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
        hold = max(.9, plan['delay']/1000 + 7/plan['rate']) if plan['rate'] else .9
        pause = max(.1, 3/plan['rate']) if plan['rate'] else .1
        if product:
            start = len(reports())
            key(1, hold); time.sleep(pause)
            after = wait(lambda s:not s['composing'])
            assert after['text'] == '左🙂右', after
            unchanged(start, True, None)
            result['checks'].append('取消组词后长按 Esc，整个期间回合持续运行、面板不打开')
            # 明显停顿后的一次真 Esc 仍可停止回合。
            key(1); wait(lambda s:s['running'] is False and s['can_rewind'])
            result['checks'].append('停顿后真实 Esc 可停止回合')
            for code in [49,23,35,30,24]: key(code)
            wait(lambda s:s['composing'])
            start = len(reports())
            key(1, hold); time.sleep(pause)
            wait(lambda s:not s['composing'])
            unchanged(start, False, None)
            result['checks'].append('空闲取消组词后长按 Esc，全程没有反复开合回退菜单')
            key(1); key(1); wait(lambda s:s['panel']=='rewind')
            result['checks'].append('空闲时真的连按两次 Esc 打开回退菜单')
            time.sleep(plan['delay']/1000 + pause)
            start = len(reports())
            key(1, hold); time.sleep(pause)
            wait(lambda s:s['panel'] is None)
            panels = [v['native_controls']['panel'] for v in reports()[start:] if 'native_controls' in v]
            assert panels and all(p is None for p in panels), panels
            result['checks'].append('回退菜单打开时长按 Esc 只关闭一次，重复不重新打开')
            key(1); key(1); wait(lambda s:s['panel']=='rewind')
            result['checks'].append('长按结束后真实双 Esc 仍能再打开回退菜单')
            for gap in [.18, .42]:
                key(1); wait(lambda s:s['panel'] is None)
                key(1); time.sleep(gap); key(1)
                wait(lambda s:s['panel']=='rewind')
            result['checks'].append('规格 500ms 内不同双按间隔均打开产品回退菜单')
        else:
            before = state()['escapes']
            key(1,hold); time.sleep(pause)
            after = wait(lambda s:not s['composing'])
            result['before'], result['after'] = before, after
            assert after['text']=='左🙂右' and after['escapes']==before, after
            result['checks'].append('取消组词后按住 Esc 不增加非组词计数，正文保留')
            key(1); wait(lambda s:s['escapes']==before+1)
            result['checks'].append('松开后再按一次 Esc 只增加一次')
            # 没有组词时，首个 Esc 是新按键，随后自动重复仍只能算一次。
            time.sleep(plan['delay']/1000 + pause)
            key(1, hold); time.sleep(pause)
            assert state()['escapes'] == before+2, state()
            result['checks'].append('空闲长按 Esc 只分派一次')
            # 两次真实按下间隔小于重复周期，不能当成自动重复吞掉。
            key(1); key(1)
            wait(lambda s:s['escapes']==before+4)
            result['checks'].append('空闲时真的快速连按两次 Esc 均分派')
            for gap in [.18, .42]:
                count = state()['escapes']
                key(1); time.sleep(gap); key(1)
                wait(lambda s:s['escapes']==count+2)
            result['checks'].append('规格 500ms 双 Esc 窗口内不同间隔的真实两次按下均有效')
            # 提交预编辑后，Esc 仍是新的按键。
            for code in [49,23]: key(code)
            wait(lambda s:s['composing'])
            key(57); wait(lambda s:not s['composing'] and s['text']!='左🙂右')
            count = state()['escapes']; key(1)
            wait(lambda s:s['escapes']==count+1)
            result['checks'].append('预编辑提交后立即按 Esc 有效')
            if plan['reconfigure']:
                for setting, value in [('RepeatRate','5'),('RepeatDelay','900')]:
                    subprocess.run(['kwriteconfig6','--file','kcminputrc','--group','Keyboard',
                                    '--key',setting,'--notify',value],check=True,timeout=5)
                wait(lambda _: (out/'lab.wayland.log').read_text().count('repeat_info(5, 900)') >= 2)
                plain, count = state()['text'], state()['escapes']
                for code in [49,23,35,30,24]: key(code)
                wait(lambda s:s['composing'])
                key(1,2.3); time.sleep(.6)
                wait(lambda s:not s['composing'])
                assert state()['text']==plain and state()['escapes']==count, state()
                key(1); wait(lambda s:s['escapes']==count+1)
                result['checks'].append('运行期间 KDE 配置通知改成 5Hz/900ms 后，新节奏立即生效')
        protocol = (out/'lab.wayland.log').read_text()
        assert f'repeat_info({plan["rate"]}, {plan["delay"]})' in protocol
        result['repeat_info'] = {'rate':plan['rate'], 'delay':plan['delay']}
        result['pass'] = True
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
    helper = out/'helper/private-keyboard'
    helper.parent.mkdir(parents=True,exist_ok=True)
    flags = subprocess.check_output(['pkg-config','--cflags','--libs','libei-1.0'],text=True).split()
    subprocess.run(['cc','-O2','-Wall','-Wextra','-Werror',str(Path(__file__).with_name('private_keyboard.c')),'-o',str(helper),*flags],check=True)
    with Sandbox('escape67',output=out,memory_max=3*1024**3,
                 temporary_parent='/mnt/wd_external/nd-build/tmp') as box:
        (box.root/'plan.json').write_text(json.dumps({'rate':args.repeat_rate,'delay':args.repeat_delay,'product':bool(args.socket),'socket':args.socket,'session':args.session,'reconfigure':args.reconfigure}))
        (box.root/'config/kcminputrc').write_text(f'[Keyboard]\nRepeatRate={args.repeat_rate}\nRepeatDelay={args.repeat_delay}\n')
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
            bindings=[(Path(__file__),'/test.py'),(Path(args.bin_dir)/'nd-composer-lab','/lab'),(helper,'/keyboard'),
                      *([(Path(args.bin_dir)/'nd-desktop','/nd-desktop'),(Path(args.socket).parent,str(Path(args.socket).parent))] if args.socket else [])],
            env=environment(XDG_CURRENT_DESKTOP='KDE',QT_QPA_PLATFORM='offscreen',KWIN_WAYLAND_NO_PERMISSION_CHECKS='1',XDG_SESSION_TYPE='wayland'),runtime_max=90)
        with (out/'kwin.log').open('w') as log: completed=subprocess.run(command,stdout=log,stderr=log,timeout=105)
    print((out/'result.json').read_text())
    assert completed.returncode==0, (out/'kwin.log').read_text()[-4000:]


if __name__=='__main__':
    if sys.argv[1:]==['--inner']: inner()
    else:
        parser=argparse.ArgumentParser(description=__doc__)
        parser.add_argument('--bin-dir',required=True); parser.add_argument('--output',required=True)
        parser.add_argument('--socket'); parser.add_argument('--session')
        parser.add_argument('--reconfigure',action='store_true')
        parser.add_argument('--repeat-rate',type=int,default=25)
        parser.add_argument('--repeat-delay',type=int,default=600)
        run(parser.parse_args())

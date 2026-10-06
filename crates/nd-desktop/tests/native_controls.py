#!/usr/bin/env python3
"""私有原生窗口：发送意图、撤回中 SIGKILL、冷启动草稿、Esc 面板和回合分派。"""
import argparse
import json
import os
from pathlib import Path
import subprocess
import sys
import time


def inner():
    out = Path('/sandbox/out')
    plan = json.loads(Path('/sandbox/plan.json').read_text())
    Path('/sandbox/state/ui.json').write_text(json.dumps({'selected_session':plan['session']}))
    app = None
    handles = []
    name = 'first'
    serial = 0

    def start(label):
        nonlocal app, name
        name = label
        stdout = (out / f'{name}.jsonl').open('w')
        stderr = (out / f'{name}.log').open('w')
        handles.extend([stdout, stderr])
        app = subprocess.Popen(['/nd-desktop','--socket',plan['socket'],'--state','/sandbox/state/ui.json',
            '--scenario-controls','/sandbox/action.json','--quit-after','80'],stdout=stdout,stderr=stderr)

    def events():
        values = []
        for line in (out / f'{name}.jsonl').read_text().splitlines():
            try: values.append(json.loads(line))
            except json.JSONDecodeError: pass
        return values

    def wait(predicate, timeout=30):
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            assert app.poll() is None, (out / f'{name}.log').read_text()[-4000:]
            values = events()
            if predicate(values): return values
            time.sleep(.02)
        raise AssertionError((out / f'{name}.jsonl').read_text()[-6000:])

    def state(values):
        return next((v['native_controls'] for v in reversed(values) if 'native_controls' in v), {})

    def snapshot(values):
        return next((v['rendered_session'] for v in reversed(values) if 'rendered_session' in v), {'items':[]})

    def prompt(values, text, status=None):
        return next((i for i in snapshot(values)['items'] if i['kind'] == 'prompt' and i['data']['text'] == text
            and (status is None or i['data']['state'] == status)), None)

    def action(kind, **kwargs):
        nonlocal serial
        serial += 1
        action_id = str(serial)
        path = Path('/sandbox/action.json')
        path.with_suffix('.tmp').write_text(json.dumps({'id':action_id,'action':kind,**kwargs}))
        path.with_suffix('.tmp').replace(path)
        wait(lambda vs:state(vs).get('action') == action_id)

    def screenshot(name):
        time.sleep(.25)
        result=subprocess.run(['spectacle','-b','-n','-f','-o',str(out/f'{name}.png')],capture_output=True,timeout=10,env=dict(os.environ,QT_QPA_PLATFORM='wayland'))
        assert result.returncode == 0,result.stderr.decode()

    def wait_file(name):
        deadline = time.monotonic() + 20
        while not (out / name).exists():
            assert time.monotonic() < deadline, name
            time.sleep(.02)

    try:
        start('first')
        wait(lambda vs:any(i['kind']=='header' for i in snapshot(vs)['items']))
        action('intent', intent='fold'); action('edit',text='ui fold'); action('send')
        wait(lambda vs:prompt(vs,'ui fold','written'))
        action('withdraw',text='ui fold')
        wait(lambda vs:prompt(vs,'ui fold','withdrawn') and state(vs).get('text') == 'ui fold')
        action('intent',intent='after_turn'); action('edit',text='ui later'); action('send')
        wait(lambda vs:prompt(vs,'ui later','written'))
        (out / 'arm-withdraw').touch(); wait_file('armed')
        action('withdraw',text='ui later')
        wait(lambda vs:prompt(vs,'ui later','withdrawing'))
        app.kill(); assert app.wait(timeout=5) == -9
        Path('/sandbox/action.json').unlink()
        (out / 'ui-killed').touch(); wait_file('resumed')
        start('reopened')
        cold = wait(lambda vs:state(vs).get('text') == 'ui later' and prompt(vs,'ui later','withdrawn'))
        screenshot('withdrawn-draft')
        action('intent',intent='interrupting'); action('edit',text='ui now'); action('send')
        wait(lambda vs:prompt(vs,'ui now','written'))
        (out / 'wait-now').touch(); wait_file('now-started')
        action('panel'); action('escape')
        wait(lambda vs: state(vs).get('panel') is None)
        # 收起面板的 Esc 不能同时停回合。
        assert any(i['kind']=='header' and i['data']['process']['turn_running'] for i in snapshot(events())['items'])
        action('escape')
        wait(lambda vs:any(i['kind']=='header' and not i['data']['process']['turn_running'] for i in snapshot(vs)['items']))
        action('escape'); action('escape')
        wait(lambda vs:state(vs).get('panel') == 'rewind')
        screenshot('rewind-menu')
        action('escape'); wait(lambda vs: state(vs).get('panel') is None)
        (out / 'result.json').write_text(json.dumps({'pass':True,'session':plan['session'],'cold':snapshot(cold),
            'checks':['three UI intent choices','withdraw survives SIGKILL before CLI ACK','cold editor restores durable draft',
                      'Esc closes panel before interrupt','idle double Esc opens rewind menu']},ensure_ascii=False,indent=2))
    finally:
        if app is not None and app.poll() is None: app.kill(); app.wait(timeout=5)
        for handle in handles: handle.close()


if __name__ == '__main__':
    if sys.argv[1:] == ['--inner']: inner()
    else:
        from native_chat import run
        parser=argparse.ArgumentParser(description=__doc__)
        for name in ['desktop','socket','output','session']: parser.add_argument('--'+name,required=True)
        run(parser.parse_args(), __file__)

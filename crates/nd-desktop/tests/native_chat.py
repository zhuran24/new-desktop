#!/usr/bin/env python3
"""私有 KWin 中驱动产品创建表单，流式 Markdown 中 SIGKILL 界面再冷启动。
真守护进程、CLI 和伪模型端点由 nd-daemon/tests/sessions.rs 的 Scenario 提供。
"""
import argparse
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tempfile
import time
import uuid
from PIL import Image


def inner():
    out = Path('/sandbox/out')
    settings = json.loads(Path('/sandbox/plan.json').read_text())
    socket = settings['socket']
    if settings.get('themes'):
        theme_dir = Path('/sandbox/config/new-desktop/themes')
        theme_dir.mkdir(parents=True)
        theme_file = theme_dir / 'ocean.json'
        theme_source = Path('/sandbox/ocean.json').read_text()
        theme_file.write_text(theme_source)
        Path('/sandbox/state/ui.json').write_text(json.dumps({'theme_selection': {'kind': 'file', 'file': 'ocean.json'}}))
    app = None
    handles = []
    apps = []

    def start(name, create=False, draft=None, device='ui'):
        stdout = (out / f'{name}.jsonl').open('w')
        stderr = (out / f'{name}.log').open('w')
        handles.extend([stdout, stderr])
        args = ['/nd-desktop', '--socket', socket, '--state', f'/sandbox/state/{device}.json', '--quit-after', '45']
        if create:
            args += ['--scenario-create', json.dumps({'cwd': '/sandbox/project', 'model': 'haiku', 'text': '请写代码', 'attachments': settings.get('attachments', False)})]
        if settings.get('session_settings'):
            args += ['--scenario-settings', '{}']
        if settings.get('history'):
            args += ['--scenario-history', json.dumps({'round': settings['round'], **({'hover_gate': '/sandbox/hover-ready', 'click_gate': '/sandbox/click-ready'} if settings.get('themes') else {})})]
        if draft is not None:
            args += ['--scenario-draft', json.dumps(draft)]
        process = subprocess.Popen(args, stdout=stdout, stderr=stderr, env=dict(os.environ, WAYLAND_DEBUG='client'))
        apps.append(process)
        return process

    def wait(name, predicate, timeout=40):
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if app.poll() is not None:
                raise AssertionError(f'{name} exited {app.returncode}: {(out / (name + ".log")).read_text()[-3000:]}')
            for line in (out / f'{name}.jsonl').read_text().splitlines():
                try:
                    event = json.loads(line)
                except json.JSONDecodeError:
                    continue
                value = event.get('rendered_history' if settings.get('history') else ('rendered_editor' if settings.get('drafts') else 'rendered_session'))
                if value is not None and predicate(value):
                    return value
            time.sleep(0.03)
        screenshot('timeout')
        raise AssertionError(f'{name} never reached expected state: {(out / (name + ".jsonl")).read_text()[-4000:]}')

    def block(snapshot):
        return next((i for i in snapshot['items'] if i['kind'] == 'text'), None)

    def screenshot(name):
        # Render 观测先于 KWin 呈现；等待实际非空屏幕，不能用固定延时冒充帧证据。
        # 仅为截图采样，不是输入到上屏的延迟测量。
        target = out / f'{name}.png'
        for _ in range(20):
            time.sleep(0.25)
            result = subprocess.run(['spectacle', '-b', '-n', '-f', '-o', str(target)], capture_output=True,
                                    timeout=10, env=dict(os.environ, QT_QPA_PLATFORM='wayland'))
            assert result.returncode == 0, result.stderr.decode()
            with Image.open(target) as image:
                histogram = image.convert('L').histogram()
                if sum(histogram[16:]) > image.width * image.height * 0.05:
                    return
        raise AssertionError(f'{name}: compositor only produced empty screenshots')

    try:
        if settings.get('session_settings'):
            Path('/sandbox/state/ui.json').write_text(json.dumps({'selected_session': settings['session'], **({'theme_selection': {'kind': 'file', 'file': 'ocean.json'}} if settings.get('themes') else {})}))
            app = start('settings')
            def configured(s):
                h = next((i['data'] for i in s['items'] if i['kind'] == 'header'), {})
                return h.get('model') == 'opus' and h.get('settings', {}).get('applied', {}).get('effort') == 'high' and h.get('title') == '原生设置标题'
            value = wait('settings', configured)
            screenshot('settings-dark')
            if settings.get('themes'):
                theme_file.write_text(theme_source.replace('#123456ff', '#26384aff').replace('"body": 18', '"body": 20'))
                deadline = time.monotonic() + 8
                while time.monotonic() < deadline:
                    events = [json.loads(line) for line in (out / 'settings.jsonl').read_text().splitlines()]
                    theme_events = [e['rendered_theme'] for e in events if 'rendered_theme' in e]
                    sessions = [e['rendered_session'] for e in events if 'rendered_session' in e]
                    if theme_events and theme_events[-1]['theme']['colors']['background'] == '#26384aff':
                        assert configured(sessions[-1]), 'theme change altered session settings/title'
                        break
                    time.sleep(.05)
                else:
                    raise AssertionError('settings theme never updated')
                screenshot('settings-new-theme')
            (out / 'result.json').write_text(json.dumps({'pass': True, 'session': value}, ensure_ascii=False))
            return
        if settings.get('history'):
            (Path('/sandbox/state') / 'ui.json').write_text(json.dumps({'selected_session': settings['session'], **({'theme_selection': {'kind': 'file', 'file': 'ocean.json'}} if settings.get('themes') else {})}))
            app = start('history')
            if settings.get('themes'):
                wait('history', lambda s: s['rounds'] == settings['rounds'] and s['messages'] > 0)
                deadline = time.monotonic() + 8
                while time.monotonic() < deadline:
                    events = [json.loads(line) for line in (out / 'history.jsonl').read_text().splitlines()]
                    if any(e.get('rendered_theme', {}).get('theme', {}).get('colors', {}).get('background') == '#123456ff' for e in events):
                        break
                    time.sleep(.05)
                screenshot('before-hover')
                Path('/sandbox/hover-ready').touch()
                deadline = time.monotonic() + 8
                while time.monotonic() < deadline:
                    if any('history_preview' in json.loads(line) for line in (out / 'history.jsonl').read_text().splitlines()):
                        break
                    time.sleep(.05)
                screenshot('themed-tooltip')
                with Image.open(out / 'before-hover.png') as before, Image.open(out / 'themed-tooltip.png') as after:
                    # 只有悬停弹出的区域应新增文件指定的 surface 色；聊天正文早已呈现。
                    newly_themed = sum(b != a and a == (23, 63, 95)
                        for b, a in zip(before.convert('RGB').get_flattened_data(), after.convert('RGB').get_flattened_data()))
                assert newly_themed > 1000, f'tooltip still uses a default palette: {newly_themed}'
                theme_file.write_text(theme_source.replace('#173f5fff', '#3d293fff'))
                deadline = time.monotonic() + 8
                while time.monotonic() < deadline:
                    events = [json.loads(line) for line in (out / 'history.jsonl').read_text().splitlines()]
                    if any(e.get('rendered_theme', {}).get('theme', {}).get('colors', {}).get('surface') == '#3d293fff' for e in events):
                        break
                    time.sleep(.05)
                screenshot('tooltip-hot-reload')
                with Image.open(out / 'before-hover.png') as before, Image.open(out / 'themed-tooltip.png') as shown, Image.open(out / 'tooltip-hot-reload.png') as changed:
                    repainted = sum(b != a and a == (23, 63, 95) and c == (61, 41, 63)
                        for b, a, c in zip(before.convert('RGB').get_flattened_data(), shown.convert('RGB').get_flattened_data(), changed.convert('RGB').get_flattened_data()))
                assert repainted > 1000, f'open tooltip kept the previous theme: {repainted}'
                theme_file.write_text(theme_source)
                Path('/sandbox/click-ready').touch()
            value = wait('history', lambda s: s.get('anchor') is not None)
            assert value['first'] == settings['text'], value
            assert value['rounds'] == settings['rounds'], value
            assert value['messages'] <= 60, value
            assert any(settings['text'] in json.loads(line).get('history_preview', '') for line in (out / 'history.jsonl').read_text().splitlines()), 'hover must show round preview'
            screenshot('history-dark')
            if settings.get('themes'):
                theme_file.write_text(theme_source.replace('#123456ff', '#26384aff').replace('"body": 18', '"body": 20'))
                deadline = time.monotonic() + 8
                while time.monotonic() < deadline:
                    events = [json.loads(line) for line in (out / 'history.jsonl').read_text().splitlines()]
                    theme_events = [e['rendered_theme'] for e in events if 'rendered_theme' in e]
                    history_events = [e['rendered_history'] for e in events if 'rendered_history' in e]
                    if theme_events and theme_events[-1]['theme']['colors']['background'] == '#26384aff':
                        assert history_events[-1] == value, 'theme change moved the selected history page/anchor'
                        break
                    time.sleep(.05)
                else:
                    raise AssertionError('history theme never updated')
                screenshot('history-new-theme')
            assert re.search(r'wl_surface#\d+\.attach\(wl_buffer#', (out / 'history.log').read_text())
            (out / 'result.json').write_text(json.dumps({'pass': True, 'history': value}))
            return
        if settings.get('drafts'):
            for device in ['a', 'b']:
                (Path('/sandbox/state') / f'{device}.json').write_text(json.dumps({'selected_session': settings['session']}))
            app = start('draft-a', draft={'text': 'A 的未发送草稿\n第二行 🦀'}, device='a')
            wait('draft-a', lambda s: s['text'] == 'A 的未发送草稿\n第二行 🦀' and s['saved'])
            app.kill()
            assert app.wait(timeout=5) == -9
            app = start('draft-reopened', device='a')
            wait('draft-reopened', lambda s: s['text'] == 'A 的未发送草稿\n第二行 🦀' and s['saved'])
            app = start('draft-b', draft={'text': 'B 接着修改'}, device='b')
            wait('draft-b', lambda s: s['text'] == 'B 接着修改' and s['saved'])
            wait('draft-reopened', lambda s: s['text'] == 'B 接着修改' and s['saved'])
            screenshot('draft-two-windows')
            app.terminate()
            app.wait(timeout=5)
            app = start('draft-recover', draft={'restore': 'native-loser'}, device='b')
            wait('draft-recover', lambda s: s['text'] == '可找回的落败稿' and s['saved'])
            wait('draft-reopened', lambda s: s['text'] == '可找回的落败稿' and s['saved'])
            app.terminate()
            app.wait(timeout=5)
            app = start('draft-send', draft={'text': '原生发送清稿', 'send': True}, device='b')
            # 输入、保存和受理可能在一次绘制前完成；不要求中间正文单独占一帧。
            # 首帧未加载时 saved=false，已加载的原稿非空，因此这个空稿只能来自受理清稿。
            wait('draft-send', lambda s: s['text'] == '' and s['saved'], timeout=10)
            wait('draft-reopened', lambda s: s['text'] == '' and s['saved'])
            (out / 'result.json').write_text(json.dumps({'pass': True, 'checks': [
                'native editor saves without sending', 'SIGKILL and cold reopen restore composer text',
                'two native windows follow the same daemon draft', 'saved conflict loads through the product recovery action',
                'immediate native submit waits for save, then atomically clears both editors']}))
            return
        app = start('creating', True)
        partial = wait('creating', lambda s: block(s) is not None and 'fn main' in block(s)['data']['text'] and block(s)['data']['complete'] is False)
        screenshot('streaming')
        # 等待截图期间可能又有增量。断点以截图前实际呈现的副本为下界。
        if settings.get('themes'):
            # 真实流式会话存活时，编辑主题并保留产品输入框中的未发送草稿。
            before = json.loads(subprocess.check_output(['/ndctl', '--socket', socket, 'get', partial['stream']]))
            session_id = partial['stream'].removeprefix('session/')
            command = {'id': str(uuid.uuid4()), 'device': 'theme-test', 'name': 'session.draft.update',
                       'args': {'session': session_id, 'text': '主题切换保留的草稿', 'attachments': []}, 'expect': {'draft_version': next(i['data']['version'] for i in before['items'] if i['id'] == 'draft')}}
            # 草稿经公开 nd-wire 写入；不直接改编辑器或守护进程事实。
            draft_reply = subprocess.run(['/ndctl', '--socket', socket, 'command', json.dumps(command)], capture_output=True, text=True)
            assert draft_reply.returncode == 0, draft_reply.stderr
            assert json.loads(draft_reply.stdout)['receipt']['status'] == 'done', draft_reply.stdout
            theme_file.write_text(theme_source.replace('#123456ff', '#26384aff').replace('"body": 18', '"body": 20'))
            deadline = time.monotonic() + 8
            while time.monotonic() < deadline:
                events = [json.loads(line) for line in (out / 'creating.jsonl').read_text().splitlines()]
                theme_events = [e['rendered_theme'] for e in events if 'rendered_theme' in e]
                editor_events = [e['rendered_editor'] for e in events if 'rendered_editor' in e]
                if (theme_events and theme_events[-1]['theme']['colors']['background'] == '#26384aff'
                        and editor_events and editor_events[-1]['text'] == '主题切换保留的草稿'):
                    break
                time.sleep(.05)
            else:
                raise AssertionError('live theme or draft did not update')
            screenshot('streaming-custom-theme')
            after = json.loads(subprocess.check_output(['/ndctl', '--socket', socket, 'get', partial['stream']]))
            before_process = next(i['data']['process'] for i in before['items'] if i['id'] == 'header')
            after_process = next(i['data']['process'] for i in after['items'] if i['id'] == 'header')
            assert before_process['run'] == after_process['run']
            assert before_process['backend_session'] == after_process['backend_session']
            assert next(i['data']['text'] for i in after['items'] if i['id'] == 'draft') == '主题切换保留的草稿'
        app.kill()
        assert app.wait(timeout=5) == -9
        app = start('reopened')
        cold = wait('reopened', lambda s: block(s) is not None)
        assert cold['stream'] == partial['stream']
        assert block(cold)['id'] == block(partial)['id']
        assert block(cold)['data']['text'].startswith(block(partial)['data']['text'])
        finished = wait('reopened', lambda s: block(s) is not None and block(s)['data']['complete'] is True)
        assert len([i for i in finished['items'] if i['id'] == block(partial)['id']]) == 1
        screenshot('dark')
        app.terminate()
        app.wait(timeout=5)
        state_path = Path('/sandbox/state/ui.json')
        state = json.loads(state_path.read_text())
        state['theme'] = 'light'
        state['theme_selection'] = {'kind': 'light'}
        state_path.write_text(json.dumps(state))
        app = start('light')
        light = wait('light', lambda s: block(s) is not None and block(s)['data']['complete'] is True)
        assert block(light) == block(finished)
        screenshot('light')
        if settings.get('attachments'):
            app.terminate()
            app.wait(timeout=5)
            settings['drafts'] = True
            app = start('attached-draft', draft={'text': '待发送的材料', 'file': '/sandbox/pasted.txt'})
            saved = wait('attached-draft', lambda s: s['text'] == '待发送的材料' and s['saved'] and len(s.get('attachments', [])) == 1)
            app.kill()
            assert app.wait(timeout=5) == -9
            app = start('attached-draft-reopened')
            wait('attached-draft-reopened', lambda s: s['text'] == '待发送的材料' and s['saved'] and s.get('attachments') == saved['attachments'])
            screenshot('attached-draft-reopened')
            settings['drafts'] = False
        for name in ['creating', 'reopened', 'light']:
            assert re.search(r'wl_surface#\d+\.attach\(wl_buffer#', (out / f'{name}.log').read_text()), name
        (out / 'result.json').write_text(json.dumps({'pass': True, 'session': finished['stream'].removeprefix('session/'),
            'partial': partial, 'cold': cold, 'finished': finished,
            'checks': ['native create form and composer -> nd-wire -> real CLI', 'SIGKILL during Markdown; cold snapshot includes accumulated block',
                       'final block replaces partial with the same id', 'dark/light native frames and screenshots']}, ensure_ascii=False, indent=2))
    finally:
        for process in apps:
            if process.poll() is None:
                process.kill()
                process.wait(timeout=5)
        for handle in handles:
            handle.close()


def run(args, script=None):
    themes = getattr(args, "themes", False)
    out = Path(args.output).resolve()
    out.mkdir(parents=True, exist_ok=True)
    work = Path(tempfile.mkdtemp(prefix='nd-test-chat-'))
    unit = f'nd-test-chat-{uuid.uuid4().hex}.service'
    slice_name = f'nd-test-chat{uuid.uuid4().hex}.slice'
    socket = Path(args.socket).resolve()
    try:
        for name in ['home', 'claude', 'config', 'data', 'state', 'cache', 'runtime']:
            (work / name).mkdir(mode=0o700)
        (work / 'plan.json').write_text(json.dumps({'socket': str(socket), 'session_settings': args.settings, 'drafts': args.session is not None and not args.history and not args.settings, 'session': args.session, 'history': args.history, 'round': args.round, 'text': args.text, 'rounds': args.rounds, 'attachments': args.attachments, 'themes': themes}))
        if themes:
            shutil.copy(Path(__file__).parents[2] / 'nd-view-model/tests/fixtures/ocean.json', work / 'ocean.json')
        if args.attachments:
            shutil.copy(Path(__file__).resolve().parents[2] / 'nd-daemon/tests/fixtures/preview.png', work / 'pixel.png')
            (work / 'pasted.txt').write_text('复制文件里的中文正文')
            (work / 'dropped.txt').write_text('拖入文件里的独立正文')
        (work / 'dbus.conf').write_text('<busconfig><type>session</type><listen>unix:tmpdir=/tmp</listen><policy context="default"><allow send_destination="*"/><allow receive_sender="*"/><allow own="*"/></policy></busconfig>')
        (work / 'session.sh').write_text('#!/bin/sh\nexec /usr/bin/python /scenario.py --inner\n')
        (work / 'session.sh').chmod(0o700)
        subprocess.run(['systemctl', '--user', 'set-property', '--runtime', slice_name, 'MemoryMax=2G', 'MemorySwapMax=0'], check=True)
        command = ['systemd-run', '--user', '--quiet', '--wait', '--pipe', '--collect', '--unit', unit, '--slice', slice_name,
                   '-p', 'MemoryMax=2G', '-p', 'MemorySwapMax=0', '-p', 'LimitCORE=0',
                   'bwrap', '--unshare-net', '--die-with-parent', '--new-session', '--ro-bind', '/usr', '/usr', '--ro-bind', '/etc', '/etc',
                   '--symlink', 'usr/bin', '/bin', '--symlink', 'usr/lib', '/lib', '--symlink', 'usr/lib', '/lib64', '--proc', '/proc',
                   '--ro-bind', '/sys', '/sys', '--dev', '/dev', '--dev-bind', '/dev/dri', '/dev/dri', '--tmpfs', '/tmp',
                   '--bind', str(work), '/sandbox', '--bind', str(out), '/sandbox/out', '--ro-bind', str(socket.parent), str(socket.parent),
                   '--ro-bind', str(Path(script or __file__).resolve()), '/scenario.py', '--ro-bind', str(Path(args.desktop).resolve()), '/nd-desktop',
                   '--ro-bind', str(Path(args.desktop).resolve().parent / 'ndctl'), '/ndctl', '--clearenv']
        for key, value in {'PATH': '/usr/bin', 'HOME': '/sandbox/home', 'CLAUDE_CONFIG_DIR': '/sandbox/claude',
                           'XDG_RUNTIME_DIR': '/sandbox/runtime', 'XDG_CONFIG_HOME': '/sandbox/config', 'XDG_DATA_HOME': '/sandbox/data',
                           'XDG_STATE_HOME': '/sandbox/state', 'XDG_CACHE_HOME': '/sandbox/cache', 'XDG_CURRENT_DESKTOP': 'KDE',
                           'QT_QPA_PLATFORM': 'offscreen', 'LANG': 'C.UTF-8'}.items():
            command += ['--setenv', key, value]
        command += ['dbus-run-session', '--config-file', '/sandbox/dbus.conf', '--', 'kwin_wayland', '--virtual', '--socket', 'nd-test-chat',
                    '--width', '1400', '--height', '900', '--no-lockscreen', '--no-global-shortcuts', '--no-kactivities', '--exit-with-session', '/sandbox/session.sh']
        with (out / 'kwin.log').open('w') as log:
            result = subprocess.run(command, stdout=log, stderr=log, timeout=100)
        assert result.returncode == 0, (out / 'kwin.log').read_text()[-6000:]
        assert json.loads((out / 'result.json').read_text())['pass']
        print(f'PASS native chat: {out}')
    finally:
        for action, name in [('stop', unit), ('stop', slice_name), ('reset-failed', unit), ('revert', slice_name)]:
            subprocess.run(['systemctl', '--user', action, name], capture_output=True)
        shutil.rmtree(work)
        remaining = subprocess.check_output(['systemctl', '--user', 'list-units', unit, slice_name, '--no-legend', '--plain'], text=True).strip()
        (out / 'cleanup.json').write_text(json.dumps({'unit': unit, 'slice': slice_name, 'remaining': remaining, 'temporary_root_removed': not work.exists()}, indent=2))
        assert not remaining, remaining


if __name__ == '__main__':
    if sys.argv[1:] == ['--inner']:
        inner()
    else:
        parser = argparse.ArgumentParser(description=__doc__)
        parser.add_argument('--desktop', required=True)
        parser.add_argument('--socket', required=True)
        parser.add_argument('--output', required=True)
        parser.add_argument('--attachments', action='store_true')
        parser.add_argument('--themes', action='store_true')
        parser.add_argument('--session', help='run the draft editor scenario for this session')
        parser.add_argument('--history', action='store_true')
        parser.add_argument('--settings', action='store_true')
        parser.add_argument('--round')
        parser.add_argument('--text')
        parser.add_argument('--rounds', type=int)
        run(parser.parse_args())

#!/usr/bin/env python3
"""真实主题文件 -> 真实 GPUI 帧，私有 KWin/总线和真实守护进程。"""
import argparse
import json
import os
import re
from pathlib import Path
import subprocess
import sys
import time


def inner():
    out = Path('/sandbox/out')
    themes = Path('/sandbox/config/new-desktop/themes')
    themes.mkdir(parents=True)
    ocean = Path('/ocean.json').read_text()
    (themes / 'ocean.json').write_text(ocean)
    Path('/sandbox/state/ui.json').write_text(json.dumps({'theme_selection': {'kind': 'file', 'file': 'ocean.json'}}))
    handles = []
    apps = []
    daemon = None
    portals = []
    offsets = {}
    click_after = {}

    def start(name):
        for path in ['/sandbox/controls.json', '/sandbox/escape.json']:
            Path(path).unlink(missing_ok=True)
        stdout = (out / f'{name}.jsonl').open('w')
        stderr = (out / f'{name}.log').open('w')
        handles.extend([stdout, stderr])
        app = subprocess.Popen(['/nd-desktop', '--socket', '/sandbox/daemon/runtime/nd.sock',
            '--state', '/sandbox/state/ui.json', '--scenario-theme-controls', '/sandbox/controls.json', '--scenario-controls', '/sandbox/escape.json', '--quit-after', '60'], stdout=stdout, stderr=stderr,
            env=dict(os.environ, WAYLAND_DEBUG='client'))
        apps.append(app)
        return app

    def wait(app, name, predicate, timeout=12):
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            assert app.poll() is None, (out / f'{name}.log').read_text()[-3000:]
            lines = (out / f'{name}.jsonl').read_text().splitlines()
            for index, line in enumerate(lines[offsets.get(name, 0):], offsets.get(name, 0)):
                offsets[name] = index + 1
                try:
                    theme = json.loads(line).get('rendered_theme')
                except json.JSONDecodeError:
                    continue
                if theme is not None and predicate(theme):
                    return theme
            time.sleep(.03)
        raise AssertionError('theme never rendered: ' + (out / f'{name}.jsonl').read_text()[-3000:])

    def click(app, name, control):
        deadline = time.monotonic() + 5
        while time.monotonic() < deadline:
            for line in reversed((out / f'{name}.jsonl').read_text().splitlines()[click_after.get(name, 0):]):
                try:
                    data = json.loads(line).get('theme_control')
                except json.JSONDecodeError:
                    continue
                if data and data['id'] == control:
                    nonce = time.monotonic_ns()
                    Path('/sandbox/controls.json').write_text(json.dumps({'nonce': nonce, 'click': data['center']}))
                    while time.monotonic() < deadline:
                        events = []
                        for event_line in (out / f'{name}.jsonl').read_text().splitlines():
                            try:
                                events.append(json.loads(event_line))
                            except json.JSONDecodeError:
                                pass
                        consumed = next((i for i, e in enumerate(events) if e.get('theme_input_consumed') == nonce), None)
                        if consumed is not None and any('theme_control' in e for e in events[consumed + 1:]):
                            # A theme change can move every menu row. The next
                            # click must find its own control after this receipt,
                            # even if only the menu button has painted so far.
                            click_after[name] = consumed + 1
                            return
                        assert app.poll() is None
                        time.sleep(.02)
                    raise AssertionError(f'{control}: input nonce was not consumed and painted')
            time.sleep(.03)
        raise AssertionError('missing theme control: ' + control)

    def screenshot(name, expected=None):
        from PIL import Image
        for _ in range(12):
            # 副本日志早于实际合成；只接受非空且已呈现目标配色的图像。
            time.sleep(.25)
            subprocess.run(['spectacle', '-b', '-n', '-f', '-o', str(out / f'{name}.png')], check=True,
                           capture_output=True, timeout=10, env=dict(os.environ, QT_QPA_PLATFORM='wayland'))
            with Image.open(out / f'{name}.png') as image:
                histogram = image.convert('L').histogram()
                colors = {color: count for count, color in image.convert('RGB').getcolors(image.width * image.height)}
                visible = sum(histogram[16:]) > image.width * image.height * .05
            if visible and all(colors.get(color, 0) > 1000 for color in expected or []):
                return
        raise AssertionError(f'{name}: compositor did not present the expected palette')

    def system_scheme(value):
        subprocess.run(['gsettings', 'set', 'org.gnome.desktop.interface', 'color-scheme', value], check=True,
                       capture_output=True, timeout=5)
        expected = 'uint32 ' + ('1' if value == 'prefer-dark' else '2')
        deadline = time.monotonic() + 8
        while time.monotonic() < deadline:
            result = subprocess.run(['gdbus', 'call', '--session', '--dest', 'org.freedesktop.portal.Desktop',
                '--object-path', '/org/freedesktop/portal/desktop', '--method', 'org.freedesktop.portal.Settings.Read',
                'org.freedesktop.appearance', 'color-scheme'], capture_output=True, text=True, timeout=3)
            if result.returncode == 0 and expected in result.stdout:
                return
            time.sleep(.1)
        raise AssertionError('real settings portal did not adopt ' + value + ': ' + result.stdout + result.stderr)

    try:
        config = Path('/sandbox/config/xdg-desktop-portal')
        config.mkdir()
        (config / 'portals.conf').write_text('[preferred]\ndefault=gtk\n')
        for binary, bus_name in [
            ('/usr/lib/dconf-service', 'ca.desrt.dconf'),
            ('/usr/lib/xdg-desktop-portal-gtk', 'org.freedesktop.impl.portal.desktop.gtk'),
            ('/usr/lib/xdg-desktop-portal', 'org.freedesktop.portal.Desktop'),
        ]:
            log = (out / (Path(binary).name + '.log')).open('w')
            handles.append(log)
            portal = subprocess.Popen([binary], stdout=log, stderr=log, env=dict(os.environ, GDK_BACKEND='wayland'))
            portals.append(portal)
            # The frontend may otherwise activate a second GTK backend through
            # D-Bus before the explicitly launched Wayland backend owns its name.
            deadline = time.monotonic() + 8
            while time.monotonic() < deadline:
                assert portal.poll() is None, Path(log.name).read_text()
                owner = subprocess.run(['gdbus', 'call', '--session', '--dest', 'org.freedesktop.DBus',
                    '--object-path', '/org/freedesktop/DBus', '--method', 'org.freedesktop.DBus.NameHasOwner',
                    bus_name], capture_output=True, text=True, timeout=3)
                if owner.returncode == 0 and owner.stdout.strip() == '(true,)':
                    break
                time.sleep(.03)
            else:
                raise AssertionError(bus_name + ' did not acquire its bus name: ' + Path(log.name).read_text())
        system_scheme('prefer-light')
        log = (out / 'daemon.log').open('w')
        handles.append(log)
        daemon = subprocess.Popen(['/nd-daemon', '--root', '/sandbox/daemon'], stdout=log, stderr=log)
        app = start('theme')
        original = wait(app, 'theme', lambda t: True)
        assert original['theme']['colors']['background'] == '#123456ff' and original['warning'] is None, original
        screenshot('custom', [(18, 52, 86), (23, 63, 95)])
        replacement = themes / 'replacement.tmp'
        replacement.write_text(ocean.replace('#123456ff', '#26384aff').replace('"body": 18', '"body": 20'))
        replacement.replace(themes / 'ocean.json')
        changed = wait(app, 'theme', lambda t: t['theme']['colors']['background'] == '#26384aff')
        assert changed['theme']['typography']['body'] == 20
        assert app.pid == apps[0].pid
        screenshot('replaced', [(38, 56, 74), (23, 63, 95)])
        (themes / 'ocean.json').write_text('{')
        bad = wait(app, 'theme', lambda t: t['warning'] is not None and 'ocean.json' in t['warning'])
        assert bad['theme']['colors']['background'] in ['#171a20ff', '#f5f6f8ff']
        screenshot('invalid', [(245, 246, 248), (255, 255, 255)])
        (themes / 'ocean.json').write_text(ocean)
        wait(app, 'theme', lambda t: t['warning'] is None and t['theme']['colors']['background'] == '#123456ff')
        click(app, 'theme', 'theme-menu')
        screenshot('selector')
        Path('/sandbox/escape.json').write_text(json.dumps({'id': 'close-theme-panel', 'action': 'escape'}))
        deadline = time.monotonic() + 8
        while time.monotonic() < deadline:
            events = [json.loads(line) for line in (out / 'theme.jsonl').read_text().splitlines()]
            states = [e['native_controls'] for e in events if 'native_controls' in e]
            if states and states[-1]['action'] == 'close-theme-panel' and states[-1]['panel'] is None:
                break
            time.sleep(.03)
        else:
            raise AssertionError('Esc did not close the theme panel')
        click(app, 'theme', 'theme-menu')
        click(app, 'theme', 'theme-light')
        wait(app, 'theme', lambda t: t['selection']['kind'] == 'light' and t['theme']['mode'] == 'light')
        screenshot('selected-light')
        click(app, 'theme', 'theme-menu')
        click(app, 'theme', 'theme-file-ocean.json')
        wait(app, 'theme', lambda t: t['selection'].get('file') == 'ocean.json' and t['theme']['colors']['background'] == '#123456ff')
        app.terminate()
        app.wait(timeout=5)
        app = start('reopened')
        wait(app, 'reopened', lambda t: t['selection'].get('file') == 'ocean.json' and t['theme']['colors']['background'] == '#123456ff')
        click(app, 'reopened', 'theme-menu')
        click(app, 'reopened', 'theme-system')
        wait(app, 'reopened', lambda t: t['selection']['kind'] == 'system' and t['theme']['mode'] == 'light')
        system_scheme('prefer-dark')
        wait(app, 'reopened', lambda t: t['selection']['kind'] == 'system' and t['theme']['mode'] == 'dark')
        screenshot('system-dark')
        click(app, 'reopened', 'theme-menu')
        click(app, 'reopened', 'theme-light')
        wait(app, 'reopened', lambda t: t['selection']['kind'] == 'light' and t['theme']['mode'] == 'light')
        system_scheme('prefer-light')
        wait(app, 'reopened', lambda t: t['system'] == 'light' and t['theme']['mode'] == 'light')
        system_scheme('prefer-dark')
        wait(app, 'reopened', lambda t: t['system'] == 'dark' and t['theme']['mode'] == 'light')
        app.terminate()
        app.wait(timeout=5)
        for portal in portals:
            if portal.poll() is None:
                portal.terminate()
                portal.wait(timeout=5)
        (themes / 'ocean.json').unlink()
        Path('/sandbox/state/ui.json').write_text(json.dumps({'theme_selection': {'kind': 'file', 'file': 'missing.json'}}))
        app = start('missing')
        missing = wait(app, 'missing', lambda t: True)
        assert missing['warning'] is not None and 'missing.json' in missing['warning'], missing
        (themes / 'missing.json').write_text(ocean)
        wait(app, 'missing', lambda t: t['warning'] is None and t['theme']['colors']['background'] == '#123456ff')
        (themes / 'missing.json').unlink()
        themes.rmdir()
        wait(app, 'missing', lambda t: t['warning'] is not None)
        themes.mkdir()
        (themes / 'missing.json').write_text(ocean)
        wait(app, 'missing', lambda t: t['warning'] is None and t['theme']['colors']['background'] == '#123456ff')
        for name in ['theme', 'reopened', 'missing']:
            assert re.search(r'wl_surface#\d+\.attach\(wl_buffer#', (out / f'{name}.log').read_text()), name
        assert daemon.poll() is None
        (out / 'result.json').write_text(json.dumps({'pass': True, 'checks': [
            'theme file loaded by real GPUI', 'atomic file replacement updates live appearance',
            'invalid selected file falls back visibly', 'repair restores the selected file', 'native mouse selection and cold reopen persistence', 'Esc closes the theme panel',
            'real XDG portal light/dark signals drive system selection only',
            'missing file at cold startup is visible; directory delete and recreate recovers']}, ensure_ascii=False, indent=2))
    finally:
        for process in apps + portals + [daemon]:
            if process is not None and process.poll() is None:
                process.kill()
                process.wait(timeout=5)
        for handle in handles:
            handle.close()


if __name__ == '__main__':
    if sys.argv[1:] == ['--inner']:
        inner()
    else:
        import native_smoke
        parser = argparse.ArgumentParser(description=__doc__)
        parser.add_argument('--bin-dir', required=True)
        parser.add_argument('--output', required=True)
        args = parser.parse_args()
        args.composer = False
        args.timeout = 90
        native_smoke.run(args, scenario=__file__, resources=[
            (Path(__file__).parents[2] / 'nd-view-model/tests/fixtures/ocean.json', '/ocean.json')])

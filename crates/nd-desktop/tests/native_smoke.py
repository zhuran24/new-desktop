#!/usr/bin/env python3
"""真 GPUI + 真守护进程的隔离冒烟；不使用 owner 的桌面、总线、HOME 或网络。
先以 scenarios feature 构建 nd-desktop，并构建 nd-daemon/ndctl。
"""
import argparse
import json
import os
from pathlib import Path
import re
import selectors
import subprocess
import sys
import time


def rendered(app):
    selector = selectors.DefaultSelector()
    selector.register(app.stdout, selectors.EVENT_READ)
    try:
        deadline = time.monotonic() + 15
        while time.monotonic() < deadline:
            if not selector.select(0.2):
                continue
            line = app.stdout.readline()
            if not line:
                raise AssertionError(f"desktop exited: {app.poll()}")
            value = json.loads(line)
            if "rendered_snapshot" in value:
                return value["rendered_snapshot"]
        raise AssertionError("no native rendered snapshot within 15 seconds")
    finally:
        selector.close()


def inner():
    assert os.environ["WAYLAND_DISPLAY"] == "nd-test-native"
    assert os.environ["HOME"] == "/sandbox/home"
    root = Path("/sandbox")
    out = root / "out"
    daemon = None
    app = None
    handles = []
    results = []

    def start_daemon():
        log = (out / "daemon.log").open("a")
        handles.append(log)
        process = subprocess.Popen(["/nd-daemon", "--root", "/sandbox/daemon"], stdout=log, stderr=log)
        deadline = time.monotonic() + 5
        while time.monotonic() < deadline:
            if process.poll() is not None:
                raise AssertionError("daemon exited")
            probe = subprocess.run(["/ndctl", "--socket", "/sandbox/daemon/runtime/nd.sock", "get", "global"], capture_output=True)
            if probe.returncode == 0:
                return process
            time.sleep(0.05)
        raise AssertionError("daemon never became ready")

    def authoritative():
        return json.loads(subprocess.check_output(["/ndctl", "--socket", "/sandbox/daemon/runtime/nd.sock", "get", "global"]))

    def start_app(name, seconds):
        log = (out / f"{name}.log").open("w")
        handles.append(log)
        env = dict(os.environ, WAYLAND_DEBUG="client")
        return subprocess.Popen(["/nd-desktop", "--socket", "/sandbox/daemon/runtime/nd.sock", "--state", "/sandbox/state/ui.json", "--quit-after", str(seconds)], stdout=subprocess.PIPE, stderr=log, text=True, env=env)

    def screenshot(name):
        log = (out / f"{name}-screenshot.log").open("w")
        handles.append(log)
        result = subprocess.run(["spectacle", "-b", "-n", "-f", "-o", str(out / f"{name}.png")], stdout=log, stderr=log, timeout=10, env=dict(os.environ, QT_QPA_PLATFORM="wayland"))
        assert result.returncode == 0 and (out / f"{name}.png").is_file(), f"screenshot exited {result.returncode}"

    try:
        daemon = start_daemon()
        app = start_app("cold", 60)
        first = rendered(app)
        assert first["cursor"] == 0 and first["items"] and first == authoritative()
        screenshot("dark")
        app.kill()
        assert app.wait(timeout=5) == -9
        app = start_app("reopened", 8)
        assert rendered(app) == first
        daemon.kill()
        daemon.wait(timeout=5)
        daemon = start_daemon()
        renewed = rendered(app)
        assert renewed["epoch"] != first["epoch"] and renewed["cursor"] == 0
        assert renewed == authoritative()
        assert app.wait(timeout=12) == 0
        results.append("zero-event desktop SIGKILL/reopen and live daemon SIGKILL/restart match nd-wire snapshot")
        (root / "state/ui.json").write_text(json.dumps({"theme": "light", "components": {"overview": False}}))
        app = start_app("after-restart", 4)
        assert rendered(app) == renewed
        screenshot("light")
        assert app.wait(timeout=8) == 0
        assert authoritative() == renewed
        results.append("cold reopen after daemon restart; graceful desktop close leaves daemon alive")
        for name in ["cold", "reopened", "after-restart"]:
            protocol = (out / f"{name}.log").read_text()
            assert re.search(r"wl_surface#\d+\.attach\(wl_buffer#", protocol), name
        results.append("all three real windows submitted Wayland buffers; light and dark screenshots saved")
        if os.environ.get("ND_TEST_COMPOSER") == "1":
            log = (out / "composer.log").open("w")
            handles.append(log)
            app = subprocess.Popen(["bash", "/composer-lab.sh", "/nd-composer-lab", "--quit-after", "4"], stdout=subprocess.PIPE, stderr=log,
                                   text=True, env=dict(os.environ, ND_COMPOSER_TRACE="client"))
            selector = selectors.DefaultSelector()
            selector.register(app.stdout, selectors.EVENT_READ)
            try:
                assert selector.select(15), "composer lab never rendered"
                assert json.loads(app.stdout.readline())["composer_lab_ready"]
            finally:
                selector.close()
            screenshot("composer")
            assert app.wait(timeout=8) == 0
            assert re.search(r"wl_surface#\d+\.attach\(wl_buffer#", (out / "composer.log").read_text())
            results.append("offline composer lab opened and submitted native Wayland buffers")
        (out / "result.json").write_text(json.dumps({"pass": True, "checks": results}, ensure_ascii=False, indent=2))
    finally:
        for process in [app, daemon]:
            if process is not None and process.poll() is None:
                process.kill()
                process.wait(timeout=5)
        for handle in handles:
            handle.close()


def run(args, scenario=None, resources=()):
    sys.path.insert(0, str(Path(__file__).resolve().parents[2] / 'nd-testkit/python'))
    from isolation import Sandbox
    binaries = Path(args.bin_dir).resolve()
    out = Path(args.output).resolve()
    out.mkdir(parents=True, exist_ok=True)
    with Sandbox('native', output=out) as box:
        (box.root / 'daemon').mkdir(mode=0o700)
        bindings = list(resources) + [(binaries / name, '/' + name) for name in ['nd-desktop', 'nd-daemon', 'ndctl']]
        if args.composer:
            bindings += [(binaries / 'nd-composer-lab', '/nd-composer-lab'),
                         (Path(__file__).resolve().parent.parent / 'scripts/composer-lab.sh', '/composer-lab.sh')]
        command = box.native(scenario or __file__, bindings=bindings,
            env={'ND_TEST_COMPOSER': '1' if args.composer else '0', 'QT_LOGGING_RULES': 'kwin_*.debug=true'})
        with (out / 'kwin.log').open('w') as log:
            result = subprocess.run(command, stdout=log, stderr=log, timeout=getattr(args, 'timeout', 55))
        assert result.returncode == 0, (out / 'kwin.log').read_text()[-5000:]
        verdict = json.loads((out / 'result.json').read_text())
        assert verdict['pass']
        print(json.dumps(verdict, ensure_ascii=False, indent=2))


if __name__ == "__main__":
    if sys.argv[1:] == ["--inner"]:
        inner()
    else:
        parser = argparse.ArgumentParser(description=__doc__)
        parser.add_argument("--bin-dir", required=True)
        parser.add_argument("--output", required=True)
        parser.add_argument("--composer", action="store_true", help="also open the offline composer lab")
        run(parser.parse_args())

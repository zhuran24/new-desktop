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
import shutil
import subprocess
import sys
import tempfile
import time
import uuid


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
    assert os.environ["WAYLAND_DISPLAY"] == "nd-test-ticket7"
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
        results.append("zero-event UI SIGKILL/reopen and live daemon SIGKILL/restart match nd-wire snapshot")
        (root / "state/ui.json").write_text(json.dumps({"theme": "light", "components": {"overview": False}}))
        app = start_app("after-restart", 4)
        assert rendered(app) == renewed
        screenshot("light")
        assert app.wait(timeout=8) == 0
        assert authoritative() == renewed
        results.append("cold reopen after daemon restart; graceful UI close leaves daemon alive")
        for name in ["cold", "reopened", "after-restart"]:
            protocol = (out / f"{name}.log").read_text()
            assert re.search(r"wl_surface#\d+\.attach\(wl_buffer#", protocol), name
        results.append("all three real windows submitted Wayland buffers; light and dark screenshots saved")
        (out / "result.json").write_text(json.dumps({"pass": True, "checks": results}, ensure_ascii=False, indent=2))
    finally:
        for process in [app, daemon]:
            if process is not None and process.poll() is None:
                process.kill()
                process.wait(timeout=5)
        for handle in handles:
            handle.close()


def run(args):
    binaries = Path(args.bin_dir).resolve()
    out = Path(args.output).resolve()
    out.mkdir(parents=True, exist_ok=True)
    work = Path(tempfile.mkdtemp(prefix="nd-test-ticket7-"))
    unit = f"nd-test-ticket7-{uuid.uuid4().hex}.service"
    slice_name = f"nd-test-ticket7{uuid.uuid4().hex}.slice"
    try:
        for name in ["home", "claude", "config", "data", "state", "cache", "runtime", "daemon"]:
            (work / name).mkdir(mode=0o700)
        (work / "dbus.conf").write_text('''<busconfig><type>session</type><listen>unix:tmpdir=/tmp</listen>
<policy context="default"><allow send_destination="*"/><allow receive_sender="*"/><allow own="*"/></policy></busconfig>''')
        (work / "session.sh").write_text("#!/bin/sh\nexec /usr/bin/python /scenario.py --inner\n")
        (work / "session.sh").chmod(0o700)
        subprocess.run(["systemctl", "--user", "set-property", "--runtime", slice_name, "MemoryMax=2G", "MemorySwapMax=0"], check=True)
        command = ["systemd-run", "--user", "--quiet", "--wait", "--pipe", "--collect", "--unit", unit, "--slice", slice_name,
                   "-p", "MemoryMax=2G", "-p", "MemorySwapMax=0", "-p", "LimitCORE=0", "bwrap", "--unshare-net", "--die-with-parent", "--new-session",
                   "--ro-bind", "/usr", "/usr", "--ro-bind", "/etc", "/etc", "--symlink", "usr/bin", "/bin", "--symlink", "usr/lib", "/lib", "--symlink", "usr/lib", "/lib64",
                   "--proc", "/proc", "--ro-bind", "/sys", "/sys", "--dev", "/dev", "--dev-bind", "/dev/dri", "/dev/dri", "--tmpfs", "/tmp",
                   "--bind", str(work), "/sandbox", "--bind", str(out), "/sandbox/out", "--ro-bind", str(Path(__file__).resolve()), "/scenario.py"]
        for name in ["nd-desktop", "nd-daemon", "ndctl"]:
            command += ["--ro-bind", str(binaries / name), "/" + name]
        command += ["--clearenv"]
        for key, value in {"PATH": "/usr/bin", "HOME": "/sandbox/home", "CLAUDE_CONFIG_DIR": "/sandbox/claude", "XDG_RUNTIME_DIR": "/sandbox/runtime",
                           "XDG_CONFIG_HOME": "/sandbox/config", "XDG_DATA_HOME": "/sandbox/data", "XDG_STATE_HOME": "/sandbox/state", "XDG_CACHE_HOME": "/sandbox/cache",
                           "XDG_CURRENT_DESKTOP": "KDE", "QT_QPA_PLATFORM": "offscreen", "LANG": "C.UTF-8", "QT_LOGGING_RULES": "kwin_*.debug=true"}.items():
            command += ["--setenv", key, value]
        command += ["dbus-run-session", "--config-file", "/sandbox/dbus.conf", "--", "kwin_wayland", "--virtual", "--socket", "nd-test-ticket7",
                    "--width", "1400", "--height", "900", "--no-lockscreen", "--no-global-shortcuts", "--no-kactivities", "--exit-with-session", "/sandbox/session.sh"]
        with (out / "kwin.log").open("w") as log:
            result = subprocess.run(command, stdout=log, stderr=log, timeout=55)
        assert result.returncode == 0, (out / "kwin.log").read_text()[-5000:]
        verdict = json.loads((out / "result.json").read_text())
        assert verdict["pass"]
        print(json.dumps(verdict, ensure_ascii=False, indent=2))
    finally:
        subprocess.run(["systemctl", "--user", "stop", unit], capture_output=True)
        subprocess.run(["systemctl", "--user", "stop", slice_name], capture_output=True)
        subprocess.run(["systemctl", "--user", "reset-failed", unit], capture_output=True)
        subprocess.run(["systemctl", "--user", "revert", slice_name], capture_output=True)
        shutil.rmtree(work)
        remaining = subprocess.check_output(["systemctl", "--user", "list-units", unit, slice_name, "--no-legend", "--plain"], text=True).strip()
        (out / "cleanup.json").write_text(json.dumps({"unit": unit, "slice": slice_name, "remaining": remaining, "temporary_root_removed": not work.exists()}, indent=2))
        assert not remaining, remaining


if __name__ == "__main__":
    if sys.argv[1:] == ["--inner"]:
        inner()
    else:
        parser = argparse.ArgumentParser(description=__doc__)
        parser.add_argument("--bin-dir", required=True)
        parser.add_argument("--output", required=True)
        run(parser.parse_args())

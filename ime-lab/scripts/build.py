#!/usr/bin/env python3
"""Build a pinned variant, with cargo inside a 20 GiB systemd scope."""
import argparse
import datetime
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import time
import tomllib

ROOT = Path(__file__).resolve().parents[1]
COMMITS = {'v070': '0c830f4d257e69fdd17200650533ab4ca9a40cc0',
           'main': '4c7f1350331562436df868c55ac33bebc4c6406c'}
p = argparse.ArgumentParser()
p.add_argument('variant', choices=['v070', 'main', 'all'])
p.add_argument('--resolve', action='store_true', help='allow initial lockfile resolution')
args = p.parse_args()
env = os.environ.copy()
env['CARGO_HOME'] = os.environ.get('IME_LAB_CARGO_HOME', '/tmp/gpui-research-20261004/cargo-home')
env['CARGO_TARGET_DIR'] = os.environ.get('IME_LAB_TARGET_DIR', '/tmp/gpui-research-20261004/target-clean')
target = Path(env['CARGO_TARGET_DIR']).resolve()
if not target.is_relative_to('/tmp'):
    raise SystemExit('IME_LAB_TARGET_DIR must be under /tmp')
env['CARGO_BUILD_JOBS'] = os.environ.get('IME_LAB_JOBS', '8')
sha = lambda path: hashlib.sha256(path.read_bytes()).hexdigest()
variants = ['v070', 'main'] if args.variant == 'all' else [args.variant]
for variant in variants:
    stamp = datetime.datetime.now(datetime.timezone.utc).strftime('%Y%m%dT%H%M%S%fZ')
    prefix = ROOT/'logs'/f'build-{variant}-{stamp}'
    manifest = ROOT/'variants'/variant/'Cargo.toml'
    sources = [ROOT/'src/main.rs', ROOT/'src/scenario.rs', ROOT/'build.rs', manifest,
               ROOT/f'patches/observe-{variant}.patch']
    source_hashes = {str(path.relative_to(ROOT)): sha(path) for path in sources}
    command = ['systemd-run', '--user', '--scope', '-p', 'MemoryMax=20G',
               'cargo', 'build', '--release', '--manifest-path', str(manifest)]
    if not args.resolve: command.append('--locked')
    started = time.monotonic()
    with prefix.with_suffix('.log').open('w') as logfile:
        proc = subprocess.Popen(command, env=env, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)
        for line in proc.stdout:
            print(line, end='', flush=True)
            logfile.write(line)
            logfile.flush()
        code = proc.wait()
    result = dict(variant=variant, started_utc=stamp, elapsed_seconds=round(time.monotonic()-started, 3),
                  exit_code=code, command=command, cargo_home=env['CARGO_HOME'], target_dir=str(target),
                  jobs=int(env['CARGO_BUILD_JOBS']), memory_max='20G', release=True,
                  commit=COMMITS[variant], source_sha256=source_hashes,
                  rustc=subprocess.check_output(['rustc', '--version'], text=True).strip(),
                  lockfile=str(manifest.with_name('Cargo.lock')))
    if code == 0:
        binary = ROOT/'bin'/f'ime-lab-{variant}'
        shutil.copy2(target/'release'/binary.name, binary)
        result.update(binary=str(binary), binary_bytes=binary.stat().st_size, binary_sha256=sha(binary),
                      lockfile_sha256=sha(manifest.with_name('Cargo.lock')))
        locked = tomllib.loads(manifest.with_name('Cargo.lock').read_text())
        result['gpui_pre'] = next(p['version'] for p in locked['package'] if p['name'] == 'gpui-pre')
    prefix.with_suffix('.json').write_text(json.dumps(result, indent=2)+'\n')
    if code == 0:
        (ROOT/'evidence'/f'build-{variant}.json').write_text(json.dumps(result, indent=2)+'\n')
    print(json.dumps(result), flush=True)
    if code: raise SystemExit(code)

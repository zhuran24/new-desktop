#!/usr/bin/env python3
"""owner 真机只读 CPU 采样；百分比按一个核心计。"""
import argparse
import json
import math
import os
from pathlib import Path
import time

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("pid", type=int)
parser.add_argument("--seconds", type=float, default=60)
args = parser.parse_args()
if not math.isfinite(args.seconds) or args.seconds <= 0:
    parser.error("seconds must be finite and positive")


def sample():
    fields = Path(f"/proc/{args.pid}/stat").read_text().rsplit(")", 1)[1].split()
    return int(fields[11]) + int(fields[12]), fields[19]


before, identity = sample()
start = time.monotonic()
time.sleep(args.seconds)
after, current_identity = sample()
elapsed = time.monotonic() - start
if identity != current_identity:
    raise SystemExit("PID was reused; discard sample")
percent = 100 * (after - before) / os.sysconf("SC_CLK_TCK") / elapsed
print(json.dumps({"pid": args.pid, "wall_seconds": elapsed, "cpu_percent_one_core": percent,
                  "below_one_percent": percent < 1}, indent=2))

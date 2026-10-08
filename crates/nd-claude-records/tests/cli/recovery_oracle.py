#!/usr/bin/env python3
"""Manual, offline CLI upgrade check. No proprietary source is stored here.

Extract pure recovery functions from the verified local binary, run them against
synthetic inputs, and emit inputs plus expected UUID orders. Never launches CLI.
An upgrade requires reviewing this version/hash/symbol-offset manifest first.
"""
import argparse
import hashlib
import json
import mmap
from pathlib import Path
import subprocess
import sys

from recovery_cases import cases as targeted_cases

CLI = Path('/mnt/wd_external/nd-build/cli/claude-2.1.289')
SHA256 = 'a186b99e4a9c88366cd49df2f7dad56c61fc306ef0140b19ee64b7c42a8d1348'
SYMBOLS = {
    'N': 200117203, 'AP': 210466243, 'qfe': 210466315,
    'dtr': 210466508, 'DL': 210466570, 'Mb': 210466755,
    'JH': 210466819, 'Vfe': 210466896, 'DSn': 211877140,
    'pye': 211877219, 'RN': 211877306, 'NSn': 211877399,
    'kye': 211877547, 'mAr': 211878079,
}


def extract(binary, symbol, offset):
    """Balanced function body; fail closed on unsupported lexical constructs."""
    prefix = f'function {symbol}('.encode()
    assert binary[offset:offset + len(prefix)] == prefix, (symbol, offset)
    start = binary.find(b'{', offset)
    depth, quote, escaped = 0, None, False
    for pos in range(start, len(binary)):
        char = binary[pos]
        if quote is not None:
            if escaped:
                escaped = False
            elif char == ord('\\'):
                escaped = True
            elif char == quote:
                quote = None
            elif quote == ord('`') and binary[pos:pos + 2] == b'${':
                raise ValueError('template interpolation requires extractor review')
        elif char in b'\'"`':
            quote = char
        elif char == ord('/'):
            raise ValueError('comment/regexp/division requires extractor review')
        elif char == ord('{'):
            depth += 1
        elif char == ord('}'):
            depth -= 1
            if depth == 0:
                return binary[offset:pos + 1].decode()
    raise ValueError(f'unterminated function {symbol}')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--corpus', type=Path, action='append', default=[])
    parser.add_argument('--cli', type=Path, default=CLI)
    args = parser.parse_args()
    with args.cli.open('rb') as file:
        binary = mmap.mmap(file.fileno(), 0, access=mmap.ACCESS_READ)
        assert hashlib.sha256(binary).hexdigest() == SHA256, 'CLI pin mismatch; review manifest'
        extracted = [extract(binary, name, offset) for name, offset in SYMBOLS.items()]
    # Harness-only environment: enable the call-ID recovery gate and discard
    # telemetry/diagnostic bookkeeping. All ordering predicates are extracted.
    harness = 'const wf=()=>true, i=()=>{}, wG=()=>{};\n'
    harness += '\n'.join(extracted)
    harness += r'''
const fs = require('fs');
for (const line of fs.readFileSync(0, 'utf8').trim().split('\n')) {
    const test = JSON.parse(line);
    const records = new Map(test.rows.filter(row => row.uuid).map(row => [row.uuid, row]));
    const chain = [], visited = new Set();
    let row = records.get(test.leaf);
    while (row) {
        if (visited.has(row.uuid)) throw Error('cyclic fixture');
        visited.add(row.uuid);
        chain.push(row);
        row = records.get(row.parentUuid);
    }
    chain.reverse();
    test.expected = mAr(records, chain, visited).map(row => row.uuid);
    console.log(JSON.stringify(test));
}
'''
    tests = []
    for path in args.corpus:
        tests.extend(json.loads(line) for line in path.read_text().splitlines())
    targeted = list(targeted_cases())
    tests.extend(targeted)
    result = subprocess.run(
        ['node', '-e', harness],
        input=''.join(json.dumps(test) + '\n' for test in tests),
        text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
        env={'PATH': '/usr/bin:/bin'}, check=True, timeout=60,
    )
    assert len(result.stdout.splitlines()) == len(tests)
    print(json.dumps({'cli_sha256': SHA256, 'total': len(tests), 'targeted': len(targeted),
                     'extracted_sha256': {name: hashlib.sha256(code.encode()).hexdigest()
                                          for name, code in zip(SYMBOLS, extracted)}}), file=sys.stderr)
    sys.stdout.write(result.stdout)


if __name__ == '__main__':
    main()

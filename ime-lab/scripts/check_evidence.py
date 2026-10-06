#!/usr/bin/env python3
"""Independently check delivered binaries, locks, smoke runs and JSONL assertions."""
import hashlib
import json
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
sha = lambda path: hashlib.sha256(path.read_bytes()).hexdigest()
results = []
for variant in ('v070', 'main'):
    build = json.loads((ROOT/f'evidence/build-{variant}.json').read_text())
    assert build['exit_code'] == 0 and build['release'] and '--locked' in build['command']
    assert sha(Path(build['binary'])) == build['binary_sha256']
    assert sha(Path(build['lockfile'])) == build['lockfile_sha256']
    for source, digest in build['source_sha256'].items():
        assert sha(ROOT/source) == digest, source
    smoke = json.loads((ROOT/f'evidence/smoke-{variant}-guard0-none.json').read_text())
    assert smoke['pass'] and smoke['processes_left'] == []
    assert smoke['binary_sha256'] == build['binary_sha256']
    assert smoke['render_alive_ms'] >= 5000 and smoke['screenshot_exists']
    for guard in (0, 1):
        result = json.loads((ROOT/f'evidence/smoke-{variant}-guard{guard}-check.json').read_text())
        events = [json.loads(line) for line in (Path(result['output'])/'events.jsonl').read_text().splitlines()]
        assert [e['seq'] for e in events] == list(range(1, len(events)+1))
        assert all(e['schema'] == 1 and isinstance(e['ts_ms'], int) for e in events)
        for key in ['shift-enter', 'ctrl-enter', 'enter']:
            assert any(e['event'] == 'key_down' and e['keystroke'] == key for e in events)
        composed = [e for e in events if e['event'] == 'send' and e['state']['has_preedit']]
        blocked = [e for e in events if e['event'] == 'send_blocked']
        assert (len(composed), len(blocked)) == ((1, 0) if not guard else (0, 1))
        assert any(e['event'] == 'preedit_update' and e['requested_selection_utf16'] == {'start': 2, 'end': 2} for e in events)
        assert any(e['event'] == 'text_commit' and e['text'] == '你' and not e['after']['has_preedit'] for e in events)
        assert result['pass'] and result['processes_left'] == []
        results.append({'variant': variant, 'guard': guard, 'events': len(events),
                        'composition_sends': len(composed), 'blocked_sends': len(blocked), 'pass': True})
output = {'pass': True, 'checks': results}
(ROOT/'evidence/check-evidence.json').write_text(json.dumps(output, indent=2)+'\n')
print(json.dumps(output, indent=2))

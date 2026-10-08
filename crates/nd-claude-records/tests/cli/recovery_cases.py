"""Synthetic branch coverage for record recovery; contains no CLI source."""
from copy import deepcopy
from itertools import product
import random


def row(uuid, parent, kind='assistant', content='', message=None, **fields):
    return dict(type=kind, uuid=uuid, parentUuid=parent, isSidechain=False,
                timestamp='2026-10-08T00:00:00Z',
                message=dict(id=message or ('msg_' + uuid), role=kind, content=content), **fields)


def assistant(uuid, parent, message, *calls):
    return row(uuid, parent, message=message,
               content=[dict(type='tool_use', id=call, name='Read', input={}) for call in calls])


def result(uuid, parent, *calls, **fields):
    return row(uuid, parent, 'user',
               [dict(type='tool_result', tool_use_id=call, content='ok') for call in calls], **fields)


def meta(uuid, parent, kind):
    return row(uuid, parent, kind, 'metadata', **({'isMeta': True} if kind == 'user' else {}))


def finish(name, rows):
    return dict(name='targeted/' + name, leaf='done', rows=rows + [
        dict(type='last-prompt', leafUuid='done', explicit=True)])


def cases():
    # Every transparent kind, root type, ambiguous/linear tail, raw sidechain
    # value, and file position. Consecutive metadata exercises anchored insertion.
    for kind, anchor, shape, side in product(
            ['attachment', 'system', 'user'], ['a', 'r', 'm1', 'm2'],
            ['linear', 'fork', 'blocked', 'boundary'], ['same', 'absent', 'true']):
        rows = [row('u', None, 'user', 'read'), assistant('a', 'u', 'batch', 't'),
                result('r', 'a', 't'), meta('m1', 'r', 'user'), meta('m2', 'm1', 'user'),
                row('done', 'm2', content='done')]
        extra = [meta('x', anchor, kind), meta('y', 'x', kind)]
        if shape == 'fork':
            extra.append(meta('z', 'x', kind))
        if shape == 'blocked':
            extra.append(row('z', 'y', 'user', 'ordinary prompt'))
        if shape == 'boundary':
            extra.append(row('z', 'y', 'system', subtype='compact_boundary'))
        # A compact_boundary changes the parser's selection/preservation as
        # well. Keep it before the ordinary rows and explicitly pin the leaf.
        if side == 'absent':
            for value in extra:
                value.pop('isSidechain')
        if side == 'true':
            for value in extra:
                value['isSidechain'] = True
        for pos in [0, 2, 4, 6]:
            yield finish(f'tails/{kind}/{anchor}/{shape}/{side}/{pos}', rows[:pos] + extra + rows[pos:])

    # Direct and stale parent results, source UUID fallback, last chunk ownership,
    # duplicate call claims across messages, and fallback agent/sidechain checks.
    for parent, source, owner, scope, duplicate in product(
            ['a1', 'a2', 'u', 'missing'], [None, 'a1', 'a2'],
            ['same-message', 'different-message', 'shared-call'],
            ['same', 'sidechain', 'agent'], [False, True]):
        a1 = assistant('a1', 'u', 'batch', 't1')
        a2 = assistant('a2', 'a1', 'batch' if owner != 'different-message' else 'other',
                       't1' if owner == 'shared-call' else 't2')
        r2 = result('r2', parent, 't1' if owner == 'shared-call' else 't2')
        if source:
            r2['sourceToolAssistantUUID'] = source
        if scope == 'sidechain':
            r2['isSidechain'] = True
        elif scope == 'agent':
            r2['agentId'] = 'other-agent'
        rows = [row('u', None, 'user', 'read'), a1, a2, result('r1', 'a1', 't1'), r2,
                meta('x', 'r2', 'attachment'), meta('y', 'x', 'system'),
                row('done', 'r1', content='done')]
        if duplicate:
            rows.insert(4, result('duplicate', 'u', 't2'))
        name = f'parallel/{parent}/{source}/{owner}/{scope}/{duplicate}'
        yield finish(name, rows)
        shuffled = deepcopy(rows)
        random.Random(name).shuffle(shuffled)
        yield finish(name + '/shuffled', shuffled)

    # Grouping deliberately ignores sidechain/agent. Tail checks ignore agent,
    # but distinguish missing/false; fallback call IDs are globally ambiguous.
    for scope, call, order in product(['sidechain', 'agent', 'both'], ['t', 'other'], range(6)):
        other = assistant('a2', 'u', 'batch', call)
        tail = meta('x', 'a2', 'attachment')
        if scope in ['sidechain', 'both']:
            other['isSidechain'] = tail['isSidechain'] = True
        if scope in ['agent', 'both']:
            other['agentId'] = 'another-agent'
            tail['agentId'] = 'different-tail-agent'
        rows = [row('u', None, 'user'), assistant('a1', 'u', 'batch', 't'), other,
                result('r', 'a2', call), tail, row('done', 'a1')]
        random.Random(order).shuffle(rows)
        yield finish(f'group-scope/{scope}/{call}/{order}', rows)

    # Interleaved message IDs create overlapping recovery windows; a second
    # metadata anchor can be reached by both windows and must appear once.
    for order in range(80):
        rows = [row('u', None, 'user'), assistant('a1', 'u', 'A', 't1'),
                assistant('b1', 'a1', 'B', 't2'), assistant('a2', 'b1', 'A', 't3'),
                result('r1', 'a1', 't1'), result('r2', 'b1', 't2'), result('r3', 'u', 't3'),
                meta('m1', 'a2', 'user'), meta('m2', 'm1', 'system'),
                meta('x', 'm1', 'attachment'), meta('y', 'x', 'user'),
                meta('z', 'r3', 'attachment'), row('done', 'm2')]
        random.Random(order).shuffle(rows)
        yield finish(f'overlap/{order}', rows)

    yield finish('no-assistant', [row('done', None, 'user')])
    for missing in [None, '']:
        a = assistant('a', 'u', 'placeholder', 't')
        a['message']['id'] = missing
        yield finish(f'empty-message-id/{missing}', [row('u', None, 'user'), a,
                     result('r', 'a', 't'), meta('x', 'r', 'attachment'), row('done', 'r')])

    # Ambiguous call IDs cannot be disambiguated by agent, sidechain or source
    # UUID. A source UUID can still associate an already-selected result as a
    # tail root. Missing IDs in tool blocks affect predicates but are not calls.
    for scope, selected, source, malformed in product(
            ['same', 'agent', 'sidechain'], [False, True], [None, 'a'], [False, True]):
        a = assistant('a', 'u', 'A', 't')
        b = assistant('b', 'u', 'B', 't')
        r = result('r', 'u', 't')
        if scope == 'agent':
            b['agentId'] = 'other'
        if scope == 'sidechain':
            b['isSidechain'] = True
        if source:
            r['sourceToolAssistantUUID'] = source
        if malformed:
            a['message']['content'] = [dict(type='tool_use')]
            r['message']['content'].append(dict(type='tool_result'))
        rows = [row('u', None, 'user'), a, b, r, meta('x', 'r', 'attachment'),
                row('done', 'r' if selected else 'a')]
        if selected:
            r['parentUuid'] = 'a'
        yield finish(f'call-ambiguity/{scope}/{selected}/{source}/{malformed}', rows)

    for side in [None, False, True, 'absent']:
        for agent in [None, 'other', 'absent']:
            rows = [row('u', None, 'user'), assistant('a', 'u', 'A', 't'),
                    result('r', 'a', 't'), meta('m1', 'r', 'user'), meta('m2', 'm1', 'user'),
                    meta('x', 'm1', 'attachment'), meta('y', 'x', 'attachment'),
                    meta('z', 'm1', 'attachment'), row('done', 'm2')]
            for item in rows[5:8]:
                if side == 'absent':
                    item.pop('isSidechain')
                else:
                    item['isSidechain'] = side
                if agent != 'absent':
                    item['agentId'] = agent
            yield finish(f'raw-tail-fields/{side}/{agent}', rows)

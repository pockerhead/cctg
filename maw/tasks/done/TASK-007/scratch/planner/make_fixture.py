# Builds subagent_handback.jsonl + subagent_handback.meta.json for TASK-007 from real record SHAPES of
# this project's subagent transcripts. Same method as TASK-005/006 make_fixture(s).py: every string value
# is redacted, key names and JSON types are kept, semantic fields are restored with fixed fake values.
# The subagent is the one spawned by final_answer.jsonl (Agent toolu_demo32 -> agentId a0000000000000002).
# Run: python make_fixture.py  -> writes ./fixtures/subagent_handback.{jsonl,meta.json}
import json, glob, os, re, sys

ROOT = os.path.expanduser(r'~/.claude/projects/C--Users-user-dev-cctg')
OUT = os.path.join(os.path.dirname(os.path.abspath(__file__)), 'fixtures')
os.makedirs(OUT, exist_ok=True)
SID = '00000000-0000-4000-8000-000000000000'
CWD = 'C:\\work\\demo'
AGENT = 'a0000000000000002'


def load(path):
    with open(path, encoding='utf-8') as f:
        return [json.loads(l) for l in f if l.strip()]


def redact(v):
    if isinstance(v, str):
        return 'redacted'
    if isinstance(v, list):
        return [redact(x) for x in v]
    if isinstance(v, dict):
        return {('key%d' % i if re.search(r'[\\/:.]|^toolu_', k) else k): redact(x) for i, (k, x) in enumerate(v.items())}
    return v


class Ids:
    n = 0
    prev = None

    def next(self):
        self.n += 1
        return '00000000-0000-4000-8000-%012d' % self.n


ids = Ids()


def envelope(rec, msg_id=None):
    r = redact(rec)
    r['type'] = rec['type']
    for k in ('isSidechain', 'isMeta'):
        if k in rec:
            r[k] = rec[k]
    parent = ids.prev
    if 'uuid' in rec:
        u = ids.next()
        r['uuid'] = u
        r['parentUuid'] = ids.prev
        ids.prev = u
    for k in ('sessionId', 'session_id'):
        if k in rec:
            r[k] = SID
    if 'cwd' in rec:
        r['cwd'] = CWD
    if 'gitBranch' in rec:
        r['gitBranch'] = 'main'
    if 'timestamp' in rec:
        r['timestamp'] = '2026-01-01T00:01:%02d.000Z' % (ids.n % 60)
    if 'version' in rec:
        r['version'] = '2.1.278'
    if 'agentId' in rec:
        r['agentId'] = AGENT
    if 'sourceToolAssistantUUID' in rec:
        r['sourceToolAssistantUUID'] = parent
    if isinstance(rec.get('message'), dict):
        m = r['message']
        src = rec['message']
        m['role'] = src.get('role')
        if 'model' in src:
            m['model'] = 'claude-opus-5-5'
        if 'id' in src:
            m['id'] = msg_id or 'msg_side%04d' % ids.n
        if 'type' in src:
            m['type'] = src['type']
        if 'stop_reason' in src:
            m['stop_reason'] = src['stop_reason']
    return r


recs = []
for p in sorted(glob.glob(os.path.join(ROOT, '*', 'subagents', 'agent-*.jsonl'))):
    recs += load(p)


def content(r):
    m = r.get('message')
    return m.get('content') if isinstance(m, dict) else None


def block_of(r, t, name=None):
    c = content(r)
    if not isinstance(c, list):
        return None
    return next((b for b in c if isinstance(b, dict) and b.get('type') == t and (name is None or b.get('name') == name)), None)


def first(pred):
    for r in recs:
        if pred(r):
            return r
    sys.exit('no record for predicate')


def with_block(src, block, msg_id=None, name=None):
    r = envelope(src, msg_id)
    b = redact(block_of(src, block['type'], name))
    b.update(block)
    r['message']['content'] = [b]
    return r


def user_str(meta, prefix, text):
    src = first(lambda r: r.get('type') == 'user' and isinstance(content(r), str) and bool(r.get('isMeta')) == meta
                and content(r).startswith(prefix))
    r = envelope(src)
    r['message']['content'] = text
    return r


def asst(kind, stop, name=None):
    return first(lambda r: r.get('type') == 'assistant' and r.get('isSidechain') is True
                 and block_of(r, kind, name) is not None
                 and len(content(r)) == 1 and r['message'].get('stop_reason') == stop)


out = []
# spawn prompt: always the first record, a non-meta string (538/538 real subagent files)
out.append(user_str(False, '', 'List the modules of the crate and hand back a one-line report.'))
out.append(user_str(True, '<system-reminder>',
                    '<system-reminder>\nYour final report is delivered through SubagentHandback.\n</system-reminder>'))
out.append(envelope(first(lambda r: r.get('type') == 'attachment' and r.get('isSidechain') is True)))
out.append(with_block(asst('thinking', None), {'type': 'thinking', 'thinking': 'SECRET-THINKING-MARKER subagent',
                      'signature': 'SECRET-SIGNATURE-MARKER'}, 'msg_sideA'))
out.append(with_block(asst('tool_use', 'tool_use', 'Bash'), {'type': 'tool_use', 'id': 'toolu_side01', 'name': 'Bash',
                      'input': {'command': 'ls crates/transcript/src', 'description': 'List source files'}},
                      'msg_sideA', 'Bash'))
res = first(lambda r: r.get('type') == 'user' and r.get('isSidechain') is True and block_of(r, 'tool_result') is not None
            and isinstance(block_of(r, 'tool_result').get('content'), str) and isinstance(r.get('toolUseResult'), dict))
out.append(with_block(res, {'type': 'tool_result', 'tool_use_id': 'toolu_side01', 'content': 'lib.rs\nrender.rs\nsplit.rs'}))
hb = first(lambda r: r.get('type') == 'assistant' and block_of(r, 'tool_use', 'SubagentHandback') is not None
           and len(content(r)) == 1)
out.append(with_block(hb, {'type': 'tool_use', 'id': 'toolu_side02', 'name': 'SubagentHandback',
                      'input': {'message': 'Modules: lib, render, split.'}}, 'msg_sideB', 'SubagentHandback'))
hb_ids = set()
for r in recs:
    b = block_of(r, 'tool_use', 'SubagentHandback')
    if b:
        hb_ids.add(b.get('id'))
hbr = first(lambda r: r.get('type') == 'user' and block_of(r, 'tool_result') is not None
            and block_of(r, 'tool_result').get('tool_use_id') in hb_ids)
out.append(with_block(hbr, {'type': 'tool_result', 'tool_use_id': 'toolu_side02', 'content': 'Report delivered to the caller.'}))
# farewell after the handback: end_turn text (215 of 222 real handback files end like this)
out.append(with_block(asst('text', 'end_turn'), {'type': 'text', 'text': 'Report handed back.'}, 'msg_sideC'))

with open(os.path.join(OUT, 'subagent_handback.jsonl'), 'w', encoding='utf-8', newline='\n') as f:
    for r in out:
        f.write(json.dumps(r, ensure_ascii=False, separators=(',', ':')) + '\n')

# .meta.json: real key set and value types (survey_subagents.out.txt), fake values linked to final_answer.jsonl
metas = [json.load(open(p, encoding='utf-8')) for p in glob.glob(os.path.join(ROOT, '*', 'subagents', '*.meta.json'))]
want = {'agentType', 'description', 'toolUseId', 'spawnDepth', 'requestShape', 'requestNonInteractive', 'model'}
src = next(m for m in metas if set(m) == want)
meta = redact(src)
meta.update(agentType='Explore', description='Explore crate', toolUseId='toolu_demo32', spawnDepth=1,
            requestShape='background', requestNonInteractive=True, model='opus')
with open(os.path.join(OUT, 'subagent_handback.meta.json'), 'w', encoding='utf-8', newline='\n') as f:
    f.write(json.dumps(meta, ensure_ascii=False, separators=(',', ':')) + '\n')
print('records', len(out), 'meta keys', list(meta))

# Builds final_answer.jsonl for TASK-006 from real record SHAPES of this project's transcripts.
# Same method as maw/tasks/done/TASK-005/scratch/make_fixtures.py: every string value is redacted,
# key names and JSON types are kept, semantic fields are restored with fixed fake values.
# Run: python make_fixture.py  -> writes ./fixtures/final_answer.jsonl
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
        r['timestamp'] = '2026-01-01T00:00:%02d.000Z' % (ids.n % 60)
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
            m['id'] = msg_id or 'msg_demo%04d' % ids.n
        if 'type' in src:
            m['type'] = src['type']
        if 'stop_reason' in src:
            m['stop_reason'] = src['stop_reason']
    return r


recs = []
for p in sorted(glob.glob(os.path.join(ROOT, '*.jsonl'))):
    recs += load(p)


def content(r):
    m = r.get('message')
    return m.get('content') if isinstance(m, dict) else None


def block_of(r, t):
    c = content(r)
    if not isinstance(c, list):
        return None
    return next((b for b in c if isinstance(b, dict) and b.get('type') == t), None)


def first(pred):
    for r in recs:
        if pred(r):
            return r
    sys.exit('no record for predicate')


def asst(kind, stop):
    return first(lambda r: r.get('type') == 'assistant' and block_of(r, kind) is not None
                 and len(content(r)) == 1 and r['message'].get('stop_reason') == stop)


def with_block(src, block, msg_id=None):
    r = envelope(src, msg_id)
    b = redact(block_of(src, block['type']))
    b.update(block)
    r['message']['content'] = [b]
    return r


def user_text(pred, text):
    r = envelope(first(pred))
    r['message']['content'] = text
    return r


def is_str_user(r, meta, prefix=None):
    c = content(r)
    if r.get('type') != 'user' or not isinstance(c, str) or bool(r.get('isMeta')) != meta:
        return False
    return c.startswith(prefix) if prefix else not c.startswith('<')


out = []
out.append(user_text(lambda r: is_str_user(r, False), 'Check the build \U0001F680 and explain.'))
# one API response split over two records sharing message.id: intermediate text, then tool_use
out.append(with_block(asst('text', 'tool_use'), {'type': 'text', 'text': 'Let me run the tests first.'}, 'msg_demoA'))
out.append(with_block(asst('tool_use', 'tool_use'), {'type': 'tool_use', 'id': 'toolu_demo31', 'name': 'Bash',
                      'input': {'command': 'cargo test --workspace', 'description': 'Run workspace tests'}}, 'msg_demoA'))
res_src = first(lambda r: r.get('type') == 'user' and block_of(r, 'tool_result') is not None
                and isinstance(block_of(r, 'tool_result').get('content'), str) and isinstance(r.get('toolUseResult'), dict))
out.append(with_block(res_src, {'type': 'tool_result', 'tool_use_id': 'toolu_demo31', 'content': 'test result: ok. 27 passed'}))
# final response: thinking record, then end_turn text record, same message.id
out.append(with_block(asst('thinking', 'end_turn'), {'type': 'thinking', 'thinking': 'SECRET-THINKING-MARKER final',
                      'signature': 'SECRET-SIGNATURE-MARKER'}, 'msg_demoB'))
out.append(with_block(asst('text', 'end_turn'), {'type': 'text', 'text': 'All 27 tests pass. Готово.'}, 'msg_demoB'))
# meta record that brief hides, then a Telegram-originated channel prompt (isMeta true, must be shown)
out.append(user_text(lambda r: is_str_user(r, True, '<local-command-caveat>'),
                     '<local-command-caveat>Caveat: demo meta record.</local-command-caveat>'))
out.append(user_text(lambda r: is_str_user(r, True, '<channel'),
                     '<channel source="cctg" chat_id="demo" message_id="7">Explore the crate</channel>'))
# Agent call and its result carrying toolUseResult.agentId
out.append(with_block(asst('tool_use', 'tool_use'), {'type': 'tool_use', 'id': 'toolu_demo32', 'name': 'Agent',
                      'input': {'description': 'Explore crate', 'prompt': 'List the modules.', 'subagent_type': 'Explore'}}, 'msg_demoC'))
ag = first(lambda r: r.get('type') == 'user' and block_of(r, 'tool_result') is not None
           and isinstance(r.get('toolUseResult'), dict) and 'agentId' in r['toolUseResult'])
rr = envelope(ag)
rb = redact(block_of(ag, 'tool_result'))
rb.update(type='tool_result', tool_use_id='toolu_demo32',
          content=[{'type': 'text', 'text': 'Modules: lib, render, split.'}, {'type': 'text', 'text': 'agentId: ' + AGENT}])
rb.pop('is_error', None)
rr['message']['content'] = [rb]
tur = redact(ag['toolUseResult'])
tur['agentId'] = AGENT
for k in ('status', 'isAsync'):
    if k in ag['toolUseResult']:
        tur[k] = ag['toolUseResult'][k]
rr['toolUseResult'] = tur
out.append(rr)
out.append(with_block(asst('text', 'end_turn'), {'type': 'text', 'text': 'The crate has three modules.'}, 'msg_demoD'))
out.append(envelope(first(lambda r: r.get('type') == 'attachment')))

with open(os.path.join(OUT, 'final_answer.jsonl'), 'w', encoding='utf-8', newline='\n') as f:
    for r in out:
        f.write(json.dumps(r, ensure_ascii=False, separators=(',', ':')) + '\n')
print('records', len(out))

# Builds anonymized fixture slices for crates/transcript/tests/fixtures/ from real transcripts.
# Every string value is redacted; key names and JSON types are kept, so the fixtures keep the real
# record shape (unknown fields included) without any real content, path, id or token.
# Run: python make_fixtures.py  -> writes ./fixtures/*.jsonl
import json, glob, os, re, sys

ROOT = os.path.expanduser(r'~/.claude/projects/C--Users-user-dev-cctg')
OUT = os.path.join(os.path.dirname(os.path.abspath(__file__)), 'fixtures')
os.makedirs(OUT, exist_ok=True)
SID = '00000000-0000-4000-8000-000000000000'
CWD = 'C:\\work\\demo'
AGENT = 'a0000000000000001'


def load(path):
    out = []
    with open(path, encoding='utf-8') as f:
        for line in f:
            line = line.strip()
            if line:
                out.append(json.loads(line))
    return out


def redact(v):
    if isinstance(v, str):
        return 'redacted'
    if isinstance(v, list):
        return [redact(x) for x in v]
    if isinstance(v, dict):
        # keys that are paths or real tool ids are renamed, ordinary field names are kept
        return {('key%d' % i if re.search(r'[\\/:.]|^toolu_', k) else k): redact(x) for i, (k, x) in enumerate(v.items())}
    return v  # numbers, bools, null keep their real shape


class Ids:
    def __init__(self):
        self.n = 0
        self.prev = None

    def next(self):
        self.n += 1
        return '00000000-0000-4000-8000-%012d' % self.n


def envelope(rec, ids):
    """Redact a whole record, then restore the semantic fields with fixed fake values."""
    r = redact(rec)
    r['type'] = rec['type']
    for k in ('isSidechain', 'isMeta'):
        if k in rec:
            r[k] = rec[k]
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
    if 'agentId' in rec:
        r['agentId'] = AGENT
    if 'version' in rec:
        r['version'] = '2.1.278'
    if isinstance(rec.get('message'), dict):
        m = r['message']
        m['role'] = rec['message'].get('role')
        if 'model' in rec['message']:
            m['model'] = 'claude-opus-5-5'
        if 'id' in rec['message']:
            m['id'] = 'msg_demo%04d' % ids.n
        if 'type' in rec['message']:
            m['type'] = rec['message']['type']
        if 'stop_reason' in rec['message']:
            m['stop_reason'] = rec['message']['stop_reason']
    return r


def write(name, records):
    with open(os.path.join(OUT, name), 'w', encoding='utf-8', newline='\n') as f:
        for r in records:
            f.write(json.dumps(r, ensure_ascii=False, separators=(',', ':')) + '\n')


main = load(os.path.join(ROOT, '1f2c01a2-63e9-464d-a70f-4a4283d3cd8b.jsonl'))


def first(pred, recs=None):
    for r in (main if recs is None else recs):
        if pred(r):
            return r
    sys.exit('no record for predicate')


def content(r):
    m = r.get('message')
    return m.get('content') if isinstance(m, dict) else None


def block_of(r, t):
    c = content(r)
    if not isinstance(c, list):
        return None
    return next((b for b in c if isinstance(b, dict) and b.get('type') == t), None)


def is_user_prompt(r):
    c = content(r)
    return r['type'] == 'user' and isinstance(c, str) and not r.get('isMeta') and not c.startswith('<')


def ignored(t):
    return envelope(first(lambda r: r.get('type') == t), Ids())


def text_block(src, text):
    b = redact(block_of(src, 'text'))
    b.update(type='text', text=text)
    return b


# ---------- plain_text.jsonl ----------
ids = Ids()
u = first(is_user_prompt)
a = first(lambda r: r['type'] == 'assistant' and block_of(r, 'text') is not None)
recs = [ignored('permission-mode'), ignored('mode'), ignored('file-history-snapshot')]
ur = envelope(u, ids)
ur['message']['content'] = 'Summarize the build status.'
recs += [ignored('attachment'), ur, ignored('queue-operation')]
ar = envelope(a, ids)
ar['message']['content'] = [text_block(a, 'The build is green.')]
recs += [ar, ignored('system'), ignored('last-prompt'), ignored('atis-latch')]
write('plain_text.jsonl', recs)

# ---------- tool_use_result.jsonl ----------
ids = Ids()


def tool_pair(pred_use):
    use = first(lambda r: r['type'] == 'assistant' and block_of(r, 'tool_use') is not None and pred_use(block_of(r, 'tool_use')))
    tid = block_of(use, 'tool_use')['id']
    res = first(lambda r: r['type'] == 'user' and block_of(r, 'tool_result') is not None
                and block_of(r, 'tool_result').get('tool_use_id') == tid)
    return use, res


def emit_pair(use, res, tool_id, name, inp, out, is_error):
    ur = envelope(use, ids)
    ub = redact(block_of(use, 'tool_use'))
    ub.update(type='tool_use', id=tool_id, name=name, input=inp)
    ur['message']['content'] = [ub]
    rr = envelope(res, ids)
    rb = redact(block_of(res, 'tool_result'))
    rb.update(type='tool_result', tool_use_id=tool_id, content=out)
    if is_error is not None:
        rb['is_error'] = is_error
    elif 'is_error' in rb:
        rb['is_error'] = False
    rr['message']['content'] = [rb]
    if 'sourceToolAssistantUUID' in rr:
        rr['sourceToolAssistantUUID'] = ur['uuid']
    return ur, rr


recs = []
use, res = tool_pair(lambda b: b['name'] == 'Bash')
recs += emit_pair(use, res, 'toolu_demo01', 'Bash', {'command': 'cargo test', 'description': 'Run tests'},
                  'test result: ok', None)
use, res = tool_pair(lambda b: b['name'] == 'Read')
recs += emit_pair(use, res, 'toolu_demo02', 'Read', {'file_path': 'C:\\work\\demo\\src\\lib.rs'}, 'fn main() {}', None)
# error result: is_error true, toolUseResult is a plain string in real data for errors
err_res = first(lambda r: r['type'] == 'user' and block_of(r, 'tool_result') is not None
                and block_of(r, 'tool_result').get('is_error') is True)
tid = block_of(err_res, 'tool_result')['tool_use_id']
err_use = first(lambda r: r['type'] == 'assistant' and block_of(r, 'tool_use') is not None and block_of(r, 'tool_use')['id'] == tid)
ur, rr = emit_pair(err_use, err_res, 'toolu_demo10', 'Bash', {'command': 'false', 'description': 'Fail on purpose'},
                   'Exit code 1', True)
if isinstance(err_res.get('toolUseResult'), str):
    rr['toolUseResult'] = 'Error: Exit code 1'
recs += [ur, rr]
# Agent call: list-shaped tool_result content and toolUseResult.agentId
use, res = tool_pair(lambda b: b['name'] == 'Agent')
ur, rr = emit_pair(use, res, 'toolu_demo20', 'Agent',
                   {'description': 'Explore crate', 'prompt': 'List the modules.', 'subagent_type': 'Explore'},
                   [{'type': 'text', 'text': 'Async agent launched.'}, {'type': 'text', 'text': 'agentId: ' + AGENT}], None)
tur = redact(res['toolUseResult'])
tur['agentId'] = AGENT
tur['status'] = res['toolUseResult'].get('status')
tur['isAsync'] = res['toolUseResult'].get('isAsync')
rr['toolUseResult'] = tur
recs += [ur, rr]
write('tool_use_result.jsonl', recs)

# ---------- thinking_ai_title.jsonl ----------
ids = Ids()
title = first(lambda r: r.get('type') == 'ai-title')
t1 = envelope(title, Ids())
t1['aiTitle'] = 'Fix flaky parser test'
t2 = envelope(title, Ids())
t2['aiTitle'] = 'Later retitle that must be ignored'
think = first(lambda r: r['type'] == 'assistant' and block_of(r, 'thinking') is not None)
mid = think['message']['id']
text_same = next((r for r in main if r['type'] == 'assistant' and r['message'].get('id') == mid
                  and block_of(r, 'text') is not None), None)
if text_same is None:
    text_same = first(lambda r: r['type'] == 'assistant' and block_of(r, 'text') is not None)
u = first(is_user_prompt)
ur = envelope(u, ids)
ur['message']['content'] = 'Why does the parser test flake?'
tr = envelope(think, ids)
tb = redact(block_of(think, 'thinking'))
tb.update(type='thinking', thinking='SECRET-THINKING-MARKER private chain of thought', signature='SECRET-SIGNATURE-MARKER')
tr['message']['content'] = [tb]
xr = envelope(text_same, ids)
xr['message']['id'] = tr['message']['id']  # real transcripts split one API message across records
xr['message']['content'] = [text_block(text_same, 'The test depends on HashMap order.')]
write('thinking_ai_title.jsonl', [t1, ur, tr, t2, xr])

# ---------- sidechain.jsonl ----------
sub = None
for p in sorted(glob.glob(os.path.join(ROOT, '*', 'subagents', 'agent-*.jsonl'))):
    recs_p = load(p)
    if any(r.get('type') == 'assistant' and block_of(r, 'text') is not None for r in recs_p):
        sub = recs_p
        break
if sub is None:
    sys.exit('no subagent transcript with text')
ids = Ids()
su = first(lambda r: r['type'] == 'user' and r.get('isSidechain'), sub)
sa = first(lambda r: r['type'] == 'assistant' and block_of(r, 'text') is not None, sub)
sur = envelope(su, ids)
if isinstance(content(su), str):
    sur['message']['content'] = 'List the modules of the crate.'
else:
    sur['message']['content'] = [{'type': 'text', 'text': 'List the modules of the crate.'}]
sar = envelope(sa, ids)
sar['message']['content'] = [text_block(sa, 'Modules: lib, parse.')]
recs = [sur]
att = next((r for r in sub if r.get('type') == 'attachment'), None)
if att is not None:
    recs.append(envelope(att, Ids()))
recs.append(sar)
write('sidechain.jsonl', recs)

# ---------- string_content.jsonl ----------
ids = Ids()
everything = [x for p in sorted(glob.glob(os.path.join(ROOT, '*.jsonl'))) for x in load(p)]
meta = first(lambda r: r['type'] == 'user' and isinstance(content(r), str) and r.get('isMeta'))
plain = first(is_user_prompt)
arr_text = first(lambda r: r['type'] == 'user' and block_of(r, 'text') is not None, everything)
mr = envelope(meta, ids)
mr['message']['content'] = '<local-command-caveat>Caveat: demo meta record.</local-command-caveat>'
pr = envelope(plain, ids)
pr['message']['content'] = 'Привет, add a test for empty input.'
ar = envelope(arr_text, ids)
ar['message']['content'] = [text_block(arr_text, 'Array-form prompt text.')]
write('string_content.jsonl', [mr, pr, ar])
print('written to', OUT)

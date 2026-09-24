# TASK-029 probe driver (README hidden-console rule). One interactive `claude`
# in a HIDDEN console in the fixed, already trusted probe folder
# %TEMP%/cctg-t016-probe (reused: no new ~/.claude.json key). MCP servers come
# only from mcp.json (--strict-mcp-config): the probe MCP server is the claude
# child that injects the key, i.e. where `cctg agent` lives. Settings come only
# from settings.json (--setting-sources project + --settings): statusLine and
# hooks are dump.py. Only our own process tree is killed.
# Usage: python probe.py <esc_bash|esc_text|ctrlb_bash|ctrlb_agent|idle_status>
BS = chr(92)
import json, os, shutil, subprocess, sys, time, uuid
here = os.path.dirname(os.path.abspath(__file__))
label = sys.argv[1]
probe = os.path.join(os.environ['TEMP'], 'cctg-t016-probe')
os.makedirs(probe, exist_ok=True)
sid = str(uuid.uuid4())
enc = ''.join('-' if c in ':' + BS + '/ _' else c for c in os.path.realpath(probe))
tpath = os.path.join(os.path.expanduser('~'), '.claude', 'projects', enc, sid + '.jsonl')
EV = os.path.join(here, 'events.jsonl')
for f in (EV, os.path.join(here, 'mcp.log'), os.path.join(here, 'inject.flag')):
    if os.path.exists(f): os.remove(f)
strip = ("CLAUDE", "CLAUDECODE", "CLAUDE_CODE_SESSION_ID", "CLAUDE_PID", "CLAUDE_CODE_CHILD_SESSION",
         "CLAUDE_CODE_SESSION_ATTENDED", "CLAUDE_CODE_ENTRYPOINT", "CLAUDE_CODE_EXECPATH",
         "CLAUDE_CODE_MESSAGING_SOCKET", "CLAUDE_CODE_MESSAGING_TOKEN", "CLAUDE_ENV_FILE", "CLAUDE_PROJECT_DIR")
env = {k: v for k, v in os.environ.items() if k not in strip}
exe = shutil.which('claude') or 'claude.exe'
cmd = [exe, '--session-id', sid, '--setting-sources', 'project',
       '--settings', os.path.join(here, 'settings.json').replace(BS, '/'),
       '--strict-mcp-config', '--mcp-config', os.path.join(here, 'mcp.json').replace(BS, '/'),
       '--allowedTools', 'Bash', 'Agent', 'Task']
PROMPTS = {
    'esc_bash': 'Run exactly one Bash tool call in the foreground (do not run it in the background): sleep 45 && echo finished. Then reply with the word done.',
    'ctrlb_bash': 'Run exactly one Bash tool call in the foreground (do not run it in the background): sleep 45 && echo finished. When the tool call returns, reply with one short sentence quoting what the tool result said, and stop. Do not wait for anything.',
    'ctrlb_agent': 'Call the Agent tool exactly once, subagent_type general-purpose, in the foreground (run_in_background false), with the prompt: "Run the Bash command sleep 40 in the foreground, then reply with the word ok." When the Agent tool returns, reply with one short sentence quoting what the tool result said, and stop. Do not wait for anything.',
    'esc_text': 'Without using any tools, write a 1500-word essay about lighthouses.',
    'idle_status': 'Reply with the single word hi.',
}
TRIGGER = {'esc_bash': ('Bash', 4, 'esc'), 'ctrlb_bash': ('Bash', 4, 'ctrlb'),
           'ctrlb_agent': (('Agent', 'Task'), 10, 'ctrlb'), 'esc_text': (None, 6, 'esc'), 'idle_status': None}
si = subprocess.STARTUPINFO(); si.dwFlags |= subprocess.STARTF_USESHOWWINDOW; si.wShowWindow = 0
t0 = time.time()
p = subprocess.Popen(cmd, cwd=probe, env=env, creationflags=subprocess.CREATE_NEW_CONSOLE, startupinfo=si)


def screen():
    out = os.path.join(here, 'screen_%s.tmp' % label)
    subprocess.call([sys.executable, os.path.join(here, 'read_console_screen.py'), str(p.pid), out],
                    stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    try: return open(out, encoding='utf-8').read()
    except Exception: return ''


def type_(text):
    return subprocess.call([sys.executable, os.path.join(here, 'type_into_console.py'), str(p.pid), text],
                           stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)


def events():
    try:
        return [json.loads(l) for l in open(EV, encoding='utf-8') if l.strip()]
    except Exception:
        return []


log, shots = [], {}
state, t_inject = 'boot', None
try:
    deadline = time.time() + 200
    while time.time() < deadline:
        time.sleep(0.5)
        ev = events()
        now = time.time() - t0
        if state == 'boot':
            s = screen(); low = s.lower()
            if 'enter to confirm' in low and 'trust' in low:
                log.append('%.1fs trust dialog -> Enter' % now); type_('\r'); time.sleep(2); continue
            if any(e['kind'] == 'statusline' for e in ev) and ('>' in s or '❯' in s):
                time.sleep(3)
                log.append('%.1fs ready -> prompt' % (time.time() - t0))
                type_(PROMPTS[label]); time.sleep(1.0); type_('\r')
                state = 'prompted'
            continue
        if state == 'prompted':
            trig = TRIGGER[label]
            if trig is None:
                if any(e.get('hook') == 'Stop' for e in ev):
                    time.sleep(8); state = 'done'; break
                continue
            names, delay, key = trig
            if names is None:
                hit = [e for e in ev if e.get('hook') == 'UserPromptSubmit']
            else:
                names = names if isinstance(names, tuple) else (names,)
                hit = [e for e in ev if e.get('hook') == 'PreToolUse' and e.get('tool_name') in names]
            if hit:
                time.sleep(delay)
                shots['before'] = screen()[-2500:]
                open(os.path.join(here, 'inject.flag'), 'w').write(key)
                t_inject = time.time() - t0
                log.append('%.1fs trigger %s seen -> inject %s after %ds' % (t_inject, names, key, delay))
                state = 'injected'
            continue
        if state == 'injected':
            el = time.time() - t0 - t_inject
            if el > 2 and 'plus2' not in shots: shots['plus2'] = screen()[-2500:]
            if any(e.get('hook') == 'Stop' for e in ev) or el > 70:
                time.sleep(4); shots['final'] = screen()[-2500:]; state = 'done'; break
finally:
    subprocess.call(['taskkill', '/PID', str(p.pid), '/T', '/F'], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    try: os.remove(os.path.join(here, 'screen_%s.tmp' % label))
    except Exception: pass
time.sleep(1)
# transcript summary: types, block kinds, interrupt markers, no text content
trows = []
try:
    for line in open(tpath, encoding='utf-8'):
        try: r = json.loads(line)
        except Exception: continue
        m = r.get('message') or {}
        c = m.get('content')
        kinds = ','.join(b.get('type', '?') for b in c if isinstance(b, dict)) if isinstance(c, list) else ('string' if isinstance(c, str) else '')
        blob = json.dumps(c) if c is not None else ''
        marks = [k for k in ('Request interrupted', 'interrupted by user', 'background', 'Backgrounded', 'backgroundTaskId', 'run_in_background') if k in blob or k in json.dumps(r.get('toolUseResult') or '')]
        tur = r.get('toolUseResult')
        turk = sorted(tur.keys()) if isinstance(tur, dict) else None
        trows.append('%s %s kinds=%s stop=%s marks=%s tur_keys=%s' % (r.get('timestamp'), r.get('type'), kinds, m.get('stop_reason'), marks, turk))
except FileNotFoundError:
    trows.append('no transcript')
ev = events()
out = ['%s: state=%s inject_at=%s sid=%s' % (label, state, t_inject, sid[:8])] + log
out.append('--- events (t relative to start) ---')
for e in ev:
    e2 = dict(e); e2['t'] = round(e['t'] - t0, 2)
    if e2.get('session_id'): e2['session_id'] = e2['session_id'][:8]
    out.append(json.dumps(e2))
out.append('--- mcp.log ---')
try: out += [l.rstrip() for l in open(os.path.join(here, 'mcp.log'), encoding='utf-8')]
except Exception: out.append('no mcp.log')
out.append('--- transcript ---'); out += trows
for k, v in shots.items():
    out.append('--- screen %s (tail) ---' % k); out.append(v)
txt = '\n'.join(out) + '\n'
open(os.path.join(here, 'probe.%s.out.txt' % label), 'w', encoding='utf-8', newline='\n').write(txt)
for f in (EV, os.path.join(here, 'mcp.log')):
    if os.path.exists(f): os.remove(f)
print(txt[:6000])

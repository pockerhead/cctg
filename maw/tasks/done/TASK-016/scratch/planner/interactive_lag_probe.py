# Interactive lag probe (README hidden-console rule): one interactive `claude`
# in a HIDDEN console in the fixed probe folder, trust dialog answered via
# WriteConsoleInputW, prompt typed the same way; the main transcript is
# tailed every 100 ms and each record's visibility delay vs its `timestamp`
# is logged (types only, no content). Only our own process tree is killed.
BS = chr(92)
import datetime, json, os, subprocess, sys, threading, time, uuid
here = os.path.dirname(os.path.abspath(__file__))
probe = os.path.join(os.environ['TEMP'], 'cctg-t016-probe')
os.makedirs(probe, exist_ok=True)
label = sys.argv[1] if len(sys.argv) > 1 else 'i1'
sid = str(uuid.uuid4())
enc = ''.join('-' if c in ':\/ _' else c for c in os.path.realpath(probe))
path = os.path.join(os.path.expanduser('~'), '.claude', 'projects', enc, sid + '.jsonl')
strip = ("CLAUDE", "CLAUDECODE", "CLAUDE_CODE_SESSION_ID", "CLAUDE_PID", "CLAUDE_CODE_CHILD_SESSION",
         "CLAUDE_CODE_SESSION_ATTENDED", "CLAUDE_CODE_ENTRYPOINT", "CLAUDE_CODE_EXECPATH",
         "CLAUDE_CODE_MESSAGING_SOCKET", "CLAUDE_CODE_MESSAGING_TOKEN", "CLAUDE_ENV_FILE", "CLAUDE_PROJECT_DIR")
env = {k: v for k, v in os.environ.items() if k not in strip}
import shutil
exe = shutil.which('claude') or 'claude.exe'
cmd = [exe, '--session-id', sid, '--setting-sources', 'project', '--allowedTools', 'Bash']
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
rows, stop, done = [], threading.Event(), threading.Event()
def tail():
    pos, buf = 0, b''
    while not stop.is_set():
        if os.path.exists(path):
            with open(path, 'rb') as f:
                f.seek(pos); chunk = f.read()
            pos += len(chunk); buf += chunk; seen = time.time()
            while b'\n' in buf:
                line, buf = buf.split(b'\n', 1)
                try: rec = json.loads(line)
                except Exception: continue
                ts = rec.get('timestamp'); m = rec.get('message') or {}
                t = datetime.datetime.fromisoformat(ts.replace('Z', '+00:00')).timestamp() if ts else None
                c = m.get('content')
                kinds = ','.join(b.get('type', '?') for b in c if isinstance(b, dict)) if isinstance(c, list) else ('string' if isinstance(c, str) else '')
                rows.append((seen, t, rec.get('type'), kinds, m.get('stop_reason')))
                if rec.get('type') == 'assistant' and m.get('stop_reason') == 'end_turn':
                    done.set()
        time.sleep(0.1)
th = threading.Thread(target=tail, daemon=True); th.start()
log = []
try:
    state = 'boot'
    deadline = time.time() + 150
    while time.time() < deadline and not done.is_set():
        time.sleep(1.0)
        s = screen()
        low = s.lower()
        if 'enter to confirm' in low and 'trust' in low:
            sel = [l for l in s.splitlines() if l.strip().startswith('>')]
            if sel and 'yes' in sel[0].lower():
                log.append('%.1fs trust dialog, Yes selected -> Enter' % (time.time() - t0)); type_('\r'); time.sleep(2)
            else:
                log.append('%.1fs trust dialog, moving selection' % (time.time() - t0)); type_(BS + 'D'); time.sleep(0.7)
            continue
        if state == 'boot' and ('for shortcuts' in low or 'shift+tab' in low or 'try "' in low):
            time.sleep(1.5)
            log.append('%.1fs input ready -> prompt' % (time.time() - t0))
            type_('Make exactly three Bash tool calls, one per call, in order, never in parallel: sleep 3 && echo one, then sleep 3 && echo two, then echo three. Before each call write one short sentence. Then answer with the single word done.')
            time.sleep(1.0); type_('\r')
            state = 'prompted'; continue
    time.sleep(2)
    final = screen()
    open(os.path.join(here, 'screen_%s_final.txt' % label), 'w', encoding='utf-8', newline='\n').write(final[-3000:])
finally:
    stop.set()
    subprocess.call(['taskkill', '/PID', str(p.pid), '/T', '/F'], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    try: os.remove(os.path.join(here, 'screen_%s.tmp' % label))
    except Exception: pass
out = ['%s interactive: state=%s end_turn_seen=%s records=%d' % (label, state, done.is_set(), len(rows))] + log
lags = []
for seen, t, kind, kinds, stopr in rows:
    lag = seen - t if t else None
    if lag is not None and kind in ('user', 'assistant'): lags.append(lag)
    out.append('%7.2fs lag=%s %-14s %-24s stop=%s' % (seen - t0, '%.2fs' % lag if lag is not None else '  -  ', kind, kinds, stopr))
if lags:
    lags.sort(); out.append('user/assistant: n=%d min=%.2f median=%.2f max=%.2f' % (len(lags), lags[0], lags[len(lags)//2], lags[-1]))
txt = '\n'.join(out) + '\n'
open(os.path.join(here, 'lag_probe.%s.out.txt' % label), 'w', encoding='utf-8', newline='\n').write(txt)
print(txt)

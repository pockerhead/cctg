"""TASK-057 probe (Windows conhost): run the installed claude in a HIDDEN
console in the fixed trusted probe folder, no user settings, no MCP; dump
rows + attribute words (idle box, a draft, after a turn = prompt suggestion).
Kills only its own process tree."""
import json, os, shutil, subprocess, sys, time
HERE = os.path.dirname(os.path.abspath(__file__))
TYPE = os.path.join(HERE, '..', '..', '..', '..', 'done', 'TASK-040', 'scratch', 'planner', 'probe', 'type_into_console.py')
PROBE = os.path.join(os.environ['TEMP'], 'cctg-t016-probe'); os.makedirs(PROBE, exist_ok=True)
ENV = {k: v for k, v in os.environ.items() if not k.startswith('CLAUDE')}
MCP = os.path.join(HERE, 'empty_mcp.json'); json.dump({'mcpServers': {}}, open(MCP, 'w'))
CLAUDE = shutil.which('claude') or 'claude.exe'
si = subprocess.STARTUPINFO(); si.dwFlags |= subprocess.STARTF_USESHOWWINDOW; si.wShowWindow = 0
proc = subprocess.Popen([CLAUDE, '--setting-sources', 'project', '--strict-mcp-config', '--mcp-config', MCP],
                        cwd=PROBE, env=ENV, creationflags=subprocess.CREATE_NEW_CONSOLE, startupinfo=si)
pid = proc.pid
def dump(tag):
    out = os.path.join(HERE, 'conhost_%s.json' % tag)
    subprocess.call([sys.executable, os.path.join(HERE, 'console_attrs.py'), str(pid), out])
    try: return json.load(open(out, encoding='utf-8'))
    except Exception: return None
def typ(text): subprocess.call([sys.executable, TYPE, str(pid), text])
def boxed(d):
    return d and sum(1 for r in d['rows'] if r['text'].startswith('─' * 20)) >= 2
try:
    t0 = time.time()
    while time.time() - t0 < 60:
        time.sleep(1); d = dump('wait')
        txt = '\n'.join(r['text'] for r in d['rows']) if d else ''
        if 'trust' in txt.lower() and 'enter to confirm' in txt.lower(): typ('\r'); time.sleep(2); continue
        if boxed(d): break
    time.sleep(3); dump('idle')
    typ('draft'); time.sleep(1); dump('draft'); typ('\x08' * 5); time.sleep(1)
    typ('Reply with just the word ok and nothing else.\r')
    t1 = time.time()
    while time.time() - t1 < 90:
        time.sleep(3); d = dump('turn')
        txt = '\n'.join(r['text'] for r in d['rows']) if d else ''
        if 'esc to interrupt' not in txt.lower() and time.time() - t1 > 8: break
    for i in range(6):
        time.sleep(3); dump('after%d' % i)
finally:
    subprocess.call(['taskkill', '/PID', str(pid), '/T', '/F'], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)

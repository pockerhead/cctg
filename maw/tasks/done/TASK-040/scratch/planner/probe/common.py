"""TASK-040 probe helpers (TEMPORARY). README hidden-console rule: every live
claude runs in a HIDDEN console (CREATE_NEW_CONSOLE + SW_HIDE) in the fixed,
already trusted probe folder %TEMP%/cctg-t016-probe (no new ~/.claude.json
key), with CLAUDE* env stripped, MCP only from a generated mcp.json
(--strict-mcp-config) and settings only from a generated file
(--setting-sources project --settings). Only our own process tree is killed."""
BS = chr(92)
import json, os, shutil, subprocess, sys, time, uuid

HERE = os.path.dirname(os.path.abspath(__file__))
PY = sys.executable.replace(BS, '/')
PROBE = os.path.join(os.environ['TEMP'], 'cctg-t016-probe')
os.makedirs(PROBE, exist_ok=True)
EV = os.path.join(HERE, 'events.jsonl')
MCPLOG = os.path.join(HERE, 'mcp.log')
FLAG = os.path.join(HERE, 'inject.flag')
STRIP = ("CLAUDE", "CLAUDECODE", "CLAUDE_CODE_SESSION_ID", "CLAUDE_PID", "CLAUDE_CODE_CHILD_SESSION",
         "CLAUDE_CODE_SESSION_ATTENDED", "CLAUDE_CODE_ENTRYPOINT", "CLAUDE_CODE_EXECPATH",
         "CLAUDE_CODE_MESSAGING_SOCKET", "CLAUDE_CODE_MESSAGING_TOKEN", "CLAUDE_ENV_FILE", "CLAUDE_PROJECT_DIR")
ENV = {k: v for k, v in os.environ.items() if k not in STRIP}
CLAUDE = shutil.which('claude') or 'claude.exe'


def p(name):
    return os.path.join(HERE, name).replace(BS, '/')


def reset():
    for f in (EV, MCPLOG, FLAG):
        if os.path.exists(f):
            os.remove(f)


def hook_cmd(kind):
    return '"%s" "%s" %s' % (PY, p('dump.py'), kind)


def write_settings(path, statusline_kind, hooks):
    """hooks: {EventName: kind} -> one dump.py command per event."""
    s = {'statusLine': {'type': 'command', 'command': hook_cmd(statusline_kind)},
         'hooks': {ev: [{'hooks': [{'type': 'command', 'command': hook_cmd(k)}]}] for ev, k in hooks.items()}}
    tmp = path + '.tmp'
    with open(tmp, 'w', encoding='utf-8', newline='\n') as f:
        json.dump(s, f, indent=2)
    os.replace(tmp, path)


def write_mcp(path, channel):
    args = [p('probe_mcp.py')] + (['channel'] if channel else [])
    with open(path, 'w', encoding='utf-8', newline='\n') as f:
        json.dump({'mcpServers': {'probe': {'command': PY, 'args': args}}}, f, indent=2)


def base_args(settings, mcp):
    return ['--setting-sources', 'project', '--settings', settings,
            '--strict-mcp-config', '--mcp-config', mcp]


def hidden_popen(cmd, cwd=PROBE):
    si = subprocess.STARTUPINFO(); si.dwFlags |= subprocess.STARTF_USESHOWWINDOW; si.wShowWindow = 0
    return subprocess.Popen(cmd, cwd=cwd, env=ENV, creationflags=subprocess.CREATE_NEW_CONSOLE, startupinfo=si)


def screen(pid, tag):
    out = os.path.join(HERE, 'screen_%s.tmp' % tag)
    subprocess.call([sys.executable, p('read_console_screen.py'), str(pid), out],
                    stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    try:
        txt = open(out, encoding='utf-8').read()
        os.remove(out)
        return txt
    except Exception:
        return ''


def type_(pid, text):
    return subprocess.call([sys.executable, p('type_into_console.py'), str(pid), text],
                           stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)


def events():
    try:
        return [json.loads(l) for l in open(EV, encoding='utf-8') if l.strip()]
    except Exception:
        return []


def kill_tree(pid):
    subprocess.call(['taskkill', '/PID', str(pid), '/T', '/F'], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)


def wait_ready(pid, t0, log, timeout=60):
    """Answer trust (should not appear: folder is trusted) and wait for the prompt box + a statusline event."""
    end = time.time() + timeout
    while time.time() < end:
        time.sleep(0.5)
        s = screen(pid, 'ready'); low = s.lower()
        if 'enter to confirm' in low and 'trust' in low:
            log.append('%.1fs trust dialog -> Enter' % (time.time() - t0)); type_(pid, BS + 'r'); time.sleep(2); continue
        if 'enter to confirm' in low and 'development channels' in low:
            log.append('%.1fs channels dialog -> Enter' % (time.time() - t0)); type_(pid, BS + 'r'); time.sleep(2); continue
        if any(e['kind'].startswith('statusline') for e in events()) and ('❯' in s or '>' in s):
            time.sleep(2)
            log.append('%.1fs ready' % (time.time() - t0))
            return True
    log.append('%.1fs NOT ready (timeout)' % (time.time() - t0))
    return False


def ev_dump(t0):
    out = []
    for e in events():
        e2 = {k: e.get(k) for k in ('t', 'kind', 'hook', 'session_id', 'reason', 'source') if k in e}
        e2['t'] = round(e['t'] - t0, 2)
        if e2.get('session_id'):
            e2['session_id'] = e2['session_id'][:8]
        out.append(json.dumps(e2))
    return out


def mcp_log():
    try:
        return [l.rstrip() for l in open(MCPLOG, encoding='utf-8')]
    except Exception:
        return ['no mcp.log']


def new_sid():
    return str(uuid.uuid4())

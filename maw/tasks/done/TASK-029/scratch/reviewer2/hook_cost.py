# TASK-029 (reviewer2 copy of the planner script, one row added): cost of one `cctg hook ToolStatus` and one `cctg statusline`
# process (the reference build, dev profile), against a local stand-in hub
# that answers 204 at once, and against a closed port (hub down). Wall time
# per process, 20 runs each. No real hub, no real config: home, state dir
# and CLAUDE_CONFIG_DIR point into a temp folder.
# Usage: python hook_cost.py <path to cctg.exe>
import http.server, json, os, shutil, socket, statistics, subprocess, sys, tempfile, threading, time

exe = sys.argv[1]
tmp = tempfile.mkdtemp(prefix='cctg-t029-cost-')
home = os.path.join(tmp, 'home'); os.makedirs(home)
cfg = os.path.join(tmp, 'cfg'); os.makedirs(cfg)


class H(http.server.BaseHTTPRequestHandler):
    def do_POST(self):
        self.rfile.read(int(self.headers.get('Content-Length', '0')))
        self.send_response(204); self.send_header('Content-Length', '0'); self.end_headers()

    def log_message(self, *a):
        pass


srv = http.server.ThreadingHTTPServer(('127.0.0.1', 0), H)
threading.Thread(target=srv.serve_forever, daemon=True).start()
live = '127.0.0.1:%d' % srv.server_address[1]
s = socket.socket(); s.bind(('127.0.0.1', 0)); dead = '127.0.0.1:%d' % s.getsockname()[1]; s.close()

pre = json.dumps({"session_id": "s", "cwd": tmp, "transcript_path": "", "hook_event_name": "PreToolUse",
                  "tool_name": "Bash", "tool_use_id": "toolu_1",
                  "tool_input": {"command": "cargo test", "description": "Run tests"}}).encode()
sub = json.dumps({"session_id": "s", "agent_id": "a1", "hook_event_name": "PreToolUse",
                  "tool_name": "Bash", "tool_use_id": "toolu_1", "tool_input": {}}).encode()
status = json.dumps({"session_id": "s", "model": {"display_name": "Opus"},
                     "context_window": {"used_percentage": 50},
                     "rate_limits": {"five_hour": {"used_percentage": 3}}}).encode()


def env(addr, chain):
    e = {k: v for k, v in os.environ.items() if not k.startswith('CLAUDE') and not k.startswith('CCTG')}
    e.update(USERPROFILE=home, HOME=home, CCTG_HUB_SECRET='cost-secret-0123456789', CCTG_HUB_HOOK_ADDR=addr,
             CCTG_STATE_DIR=os.path.join(tmp, 'state'), CLAUDE_CONFIG_DIR=cfg)
    settings = os.path.join(cfg, 'settings.json')
    if chain:
        open(settings, 'w').write(json.dumps({"statusLine": {"type": "command", "command": "echo user-line"}}))
    elif os.path.exists(settings):
        os.remove(settings)
    return e


def run(args, stdin, addr, chain=False, n=20):
    e = env(addr, chain)
    times, out = [], b''
    for _ in range(n):
        t = time.perf_counter()
        r = subprocess.run([exe] + args, input=stdin, env=e, capture_output=True)
        times.append((time.perf_counter() - t) * 1000)
        out = r.stdout
    return statistics.median(times), max(times), out


rows = []
for label, args, stdin, addr, chain in [
    ('hook ToolStatus PreToolUse, hub up', ['hook', 'ToolStatus'], pre, live, False),
    ('hook ToolStatus PreToolUse, hub down', ['hook', 'ToolStatus'], pre, dead, False),
    ('hook ToolStatus inside a subagent (skipped, no POST)', ['hook', 'ToolStatus'], sub, dead, False),
    ('statusline own line, hub up', ['statusline'], status, live, False),
    ('statusline own line, hub down', ['statusline'], status, dead, False),
    ('statusline chained `echo`, hub up', ['statusline'], status, live, True),
    ('statusline chained `echo`, hub down', ['statusline'], status, dead, True),
]:
    med, worst, out = run(args, stdin, addr, chain)
    rows.append('%-55s median %6.1f ms  max %6.1f ms  stdout=%r' % (label, med, worst, out[:60]))
srv.shutdown()
shutil.rmtree(tmp, ignore_errors=True)
txt = '\n'.join(rows) + '\n'
open(os.path.join(os.path.dirname(os.path.abspath(__file__)), 'hook_cost.out.txt'), 'w', newline='\n').write(txt)
print(txt)

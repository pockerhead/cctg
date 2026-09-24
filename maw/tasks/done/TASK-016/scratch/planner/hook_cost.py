# Cost of one `cctg hook PostToolUse` process (dev build) per tool call:
# A) non-handback tool (today: skipped before any POST),
# B) a POST to a closed local port (hub down),
# C) a POST to a listener that answers 204 at once (hub up).
import json, os, socket, statistics, subprocess, sys, threading, time
exe = sys.argv[1]; n = int(sys.argv[2]) if len(sys.argv) > 2 else 15
env = {k: v for k, v in os.environ.items() if not k.startswith('CLAUDE')}
env.update(CCTG_HUB_SECRET='0123456789abcdef-probe', CCTG_HOST='probe')
def payload(tool):
    return json.dumps({"session_id": "5e551017-0000-4000-8000-000000000001", "cwd": os.environ['TEMP'],
                       "transcript_path": "", "hook_event_name": "PostToolUse", "tool_name": tool,
                       "tool_input": {"command": "echo hi", "message": "report"}, "agent_id": "a1"}).encode()
def run(addr, tool):
    e = dict(env, CCTG_HUB_HOOK_ADDR=addr); ts = []
    for _ in range(n):
        t = time.perf_counter()
        subprocess.run([exe, 'hook', 'PostToolUse'], input=payload(tool), env=e,
                       stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, creationflags=subprocess.CREATE_NO_WINDOW)
        ts.append((time.perf_counter() - t) * 1000)
    return ts
srv = socket.socket(); srv.bind(('127.0.0.1', 0)); srv.listen(64); live = '127.0.0.1:%d' % srv.getsockname()[1]
def serve():
    while True:
        c, _ = srv.accept(); c.settimeout(2)
        try:
            data = b''
            while b'\r\n\r\n' not in data: data += c.recv(65536)
            head, body = data.split(b'\r\n\r\n', 1)
            ln = int([l for l in head.split(b'\r\n') if l.lower().startswith(b'content-length')][0].split(b':')[1])
            while len(body) < ln: body += c.recv(65536)
            c.sendall(b'HTTP/1.1 204 No Content\r\n\r\n')
        except Exception: pass
        c.close()
threading.Thread(target=serve, daemon=True).start()
closed = socket.socket(); closed.bind(('127.0.0.1', 0)); dead = '127.0.0.1:%d' % closed.getsockname()[1]; closed.close()
out = []
for label, addr, tool in [('A skip (Bash, no POST)', live, 'Bash'), ('C POST, hub up', live, 'SubagentHandback'),
                          ('B POST, hub down', dead, 'SubagentHandback')]:
    ts = sorted(run(addr, tool))
    out.append('%-24s n=%d median=%.0f ms p90=%.0f ms max=%.0f ms' % (label, n, statistics.median(ts), ts[int(n*0.9)-1], ts[-1]))
txt = '\n'.join(out) + '\n(dev build, Windows, process spawn included)\n'
open('hook_cost.out.txt', 'w', encoding='utf-8', newline='\n').write(txt); print(txt)

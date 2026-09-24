"""TASK-040 P2 (TEMPORARY): does typing `/exit` + Enter into claude's console
from its stdio MCP child (WriteConsoleInputW, the keys.rs mechanism) exit the
TUI cleanly, and does the parent process get the exit code?

Variants (argv[1]):
  idle      idle prompt box, MCP child types "/exit", 400 ms later Enter
  one_write same, but "/exit\\r" in one WriteConsoleInputW call
  safe_idle / safe_draft: like idle / draft, but the MCP child types /exit,
            reads the input box and presses Enter only when the box is exactly
            '❯ /exit', else 5 Backspaces (the design candidate; see mcp.log)
  draft     the driver first types an unsent draft "draft text" into the box,
            then the MCP child types "/exit" + Enter (what happens to a draft?)
No model turn is used. Hooks: SessionEnd (reason) and statusLine only.
Usage: python p2_exit.py <idle|one_write|draft|safe_idle|safe_draft>   -> writes p2.<variant>.result.txt"""
import os, sys, time
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from common import *

variant = sys.argv[1] if len(sys.argv) > 1 else 'idle'
reset()
sid = new_sid()
settings, mcp = p('p2_settings.json'), p('p2_mcp.json')
write_settings(settings, 'statusline', {'SessionEnd': 'session_end', 'SessionStart': 'session_start'})
write_mcp(mcp, channel=False)
log, shots = [], {}
t0 = time.time()
proc = hidden_popen([CLAUDE, '--session-id', sid] + base_args(settings, mcp))
rc, t_inj, t_exit = None, None, None
try:
    if wait_ready(proc.pid, t0, log):
        if variant in ('draft', 'safe_draft'):
            type_(proc.pid, 'draft text'); time.sleep(1.0)
            shots['draft'] = screen(proc.pid, 'p2')[-1500:]
        with open(FLAG, 'w', encoding='utf-8') as f:
            f.write({'one_write': 'exit_one_write', 'safe_idle': 'safe_exit', 'safe_draft': 'safe_exit'}.get(variant, 'exit'))
        t_inj = time.time()
        log.append('%.1fs flag written (%s)' % (t_inj - t0, variant))
        time.sleep(1.5)
        if proc.poll() is None:
            shots['plus1_5'] = screen(proc.pid, 'p2')[-2500:]
        try:
            rc = proc.wait(timeout=12 if variant == 'safe_draft' else 30)
            t_exit = time.time()
            log.append('%.1fs claude exited rc=%s (%.1fs after flag)' % (t_exit - t0, rc, t_exit - t_inj))
        except Exception:
            shots['still_running'] = screen(proc.pid, 'p2')[-2500:]
            log.append('claude still running 30 s after flag')
finally:
    if proc.poll() is None:
        kill_tree(proc.pid)
time.sleep(1.5)
ev = events()
ends = [e for e in ev if e['kind'] == 'session_end']
out = ['P2 typed /exit (%s): sid=%s' % (variant, sid[:8])] + log
out.append('VERDICT exited=%s rc=%s session_end_reason=%s' %
           (t_exit is not None, rc, [e.get('reason') for e in ends]))
out.append('--- events ---'); out += ev_dump(t0)
out.append('--- mcp.log ---'); out += mcp_log()
for k, v in shots.items():
    out.append('--- screen %s (tail) ---' % k); out.append(v)
txt = '\n'.join(out) + '\n'
open(p('p2.%s.result.txt' % variant), 'w', encoding='utf-8', newline='\n').write(txt)
reset()
print(txt[:4000])

"""TASK-040 P1 (TEMPORARY): does a running interactive claude pick up changes to
its `--settings <file>` (a changed statusLine command, a changed hook command,
a hook for a new event) without restart? This is how claude-cctg passes cctg's
hooks and statusLine today (~/.cctg/poc/settings.json).

Phase A settings: statusLine=statusline_A, UserPromptSubmit=prompt_A.
After the session is ready the file is atomically replaced (temp + rename, as
`cctg deploy` would) with phase B: statusLine=statusline_B,
UserPromptSubmit=prompt_B, NEW Stop=stop_B. After 8 s one tiny prompt is sent.
Usage: python p1_settings.py        -> writes p1.result.txt"""
import os, sys, time
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from common import *

reset()
sid = new_sid()
settings, mcp = p('p1_settings.json'), p('p1_mcp.json')
write_settings(settings, 'statusline_A', {'UserPromptSubmit': 'prompt_A'})
write_mcp(mcp, channel=False)
log = []
t0 = time.time()
proc = hidden_popen([CLAUDE, '--session-id', sid] + base_args(settings, mcp))
t_swap, shots = None, {}
try:
    if wait_ready(proc.pid, t0, log):
        write_settings(settings, 'statusline_B', {'UserPromptSubmit': 'prompt_B', 'Stop': 'stop_B'})
        t_swap = time.time()
        log.append('%.1fs settings replaced (A -> B)' % (t_swap - t0))
        time.sleep(8)
        shots['after_swap'] = screen(proc.pid, 'p1')[-2500:]
        type_(proc.pid, 'Reply with the single word hi.'); time.sleep(1.0); type_(proc.pid, BS + 'r')
        log.append('%.1fs prompt sent' % (time.time() - t0))
        end = time.time() + 90
        while time.time() < end:
            time.sleep(0.5)
            ks = [e['kind'] for e in events()]
            if 'stop_B' in ks:
                break
        time.sleep(6)
        shots['final'] = screen(proc.pid, 'p1')[-2500:]
finally:
    kill_tree(proc.pid)
time.sleep(1)
ev = events()
after = [e['kind'] for e in ev if t_swap and e['t'] > t_swap]
sl_after = sorted(set(k for k in after if k.startswith('statusline')))
prompts = sorted(set(k for k in after if k.startswith('prompt')))
out = ['P1 settings reload: sid=%s' % sid[:8]] + log
out.append('statusline kinds after swap: %s' % sl_after)
out.append('prompt hook kinds after swap: %s' % prompts)
out.append('new Stop hook fired: %s' % ('stop_B' in after))
v_sl = 'statusline_B' in sl_after and 'statusline_A' not in [k for e in ev if t_swap and e['t'] > t_swap + 3 for k in [e['kind']]]
v_hook = prompts == ['prompt_B']
out.append('VERDICT statusLine_reloaded=%s changed_hook_reloaded=%s new_event_hook_reloaded=%s' %
           (v_sl, v_hook, 'stop_B' in after))
out.append('--- events ---'); out += ev_dump(t0)
for k, v in shots.items():
    out.append('--- screen %s (tail) ---' % k); out.append(v)
txt = '\n'.join(out) + '\n'
open(p('p1.result.txt'), 'w', encoding='utf-8', newline='\n').write(txt)
reset()
print(txt[:4000])

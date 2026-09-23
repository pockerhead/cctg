# Wall time of `cctg hook <event>` (reference build) against: a closed local
# port (Windows retries SYN after RST), a hub that accepts and never answers,
# and a live fake hub that answers 204. Home points at a temp device.env.
import os, socket, subprocess, sys, tempfile, threading, time, statistics
exe = sys.argv[1]
fx = os.path.join(os.path.dirname(__file__), 'ws', 'crates', 'cctg', 'tests', 'fixtures', 'hook')
def home_for(addr):
    d = tempfile.mkdtemp(prefix='cctg-measure-')
    os.makedirs(os.path.join(d, '.cctg'))
    open(os.path.join(d, '.cctg', 'device.env'), 'w').write(
        'CCTG_HUB_SECRET=measure-secret-0123456789\nCCTG_HUB_HOOK_ADDR=%s\nCCTG_HOST=box\n' % addr)
    return d
def run(home, event, name, k=7):
    data = open(os.path.join(fx, name + '.json'), 'rb').read()
    env = dict(os.environ, USERPROFILE=home, HOME=home)
    for v in ('CCTG_HUB_SECRET', 'CCTG_HUB_HOOK_ADDR', 'CCTG_HOST'): env.pop(v, None)
    ts = []
    for _ in range(k):
        t = time.perf_counter()
        p = subprocess.run([exe, 'hook', event], input=data, capture_output=True, env=env)
        ts.append((time.perf_counter() - t) * 1000)
        assert p.returncode == 0 and not p.stdout, (p.returncode, p.stdout)
    return 'median %.0f ms, max %.0f ms' % (statistics.median(ts), max(ts))
s = socket.socket(); s.bind(('127.0.0.1', 0)); closed = s.getsockname(); s.close()
silent = socket.socket(); silent.bind(('127.0.0.1', 0)); silent.listen(64)
held = []
threading.Thread(target=lambda: [held.append(silent.accept()) for _ in iter(int, 1)], daemon=True).start()
live = socket.socket(); live.bind(('127.0.0.1', 0)); live.listen(64)
def answer():
    while True:
        c, _ = live.accept(); c.recv(65536); c.sendall(b'HTTP/1.1 204 No Content\r\n\r\n'); c.close()
threading.Thread(target=answer, daemon=True).start()
for label, addr in [('closed port', '%s:%d' % closed), ('silent hub', '%s:%d' % silent.getsockname()), ('answering hub', '%s:%d' % live.getsockname())]:
    h = home_for(addr)
    print('%-14s SessionEnd  : %s' % (label, run(h, 'SessionEnd', 'session_end')))
    print('%-14s SessionStart: %s' % (label, run(h, 'SessionStart', 'session_start')))

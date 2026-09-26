"""TASK-057 probe (Windows conhost, hidden): how a long typed line wraps in
the input box, and a slash command with its argument hint. Nothing is
submitted: every typed text is erased again."""
import json, os, subprocess, sys, time
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
HERE = os.path.dirname(os.path.abspath(__file__))
exec(open(os.path.join(HERE, 'drive_conhost.py'), encoding='utf-8').read().split(chr(10) + 'try:')[0])
LONG = '!echo ' + ' '.join('word%02d' % i for i in range(25))  # ~180 chars
try:
    t0 = time.time()
    while time.time() - t0 < 60:
        time.sleep(1); d = dump('wait')
        if boxed(d): break
    time.sleep(3)
    typ('!'); time.sleep(0.3); typ(LONG[1:]); time.sleep(1.5); dump('wrap'); typ('\x08' * len(LONG)); time.sleep(1)
    typ('/compact'); time.sleep(1.5); dump('hint'); typ('\x08' * 8); time.sleep(1)
    typ('x' * 150); time.sleep(1.5); dump('nospace'); typ('\x08' * 150); time.sleep(1)
    dump('end')
finally:
    subprocess.call(['taskkill', '/PID', str(pid), '/T', '/F'], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
print(len(LONG))

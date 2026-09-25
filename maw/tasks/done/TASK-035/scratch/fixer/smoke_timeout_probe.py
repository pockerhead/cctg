# Checks smoke.py's lines_of + deadline loop: a silent child fails in time
# with a FAIL message instead of hanging.
import importlib.util, subprocess, sys, time, queue
spec = importlib.util.spec_from_file_location("smoke", r"C:/Users/user/dev/cctg/.github/smoke/smoke.py")
smoke = importlib.util.module_from_spec(spec); spec.loader.exec_module(smoke)
child = subprocess.Popen([sys.executable, "-c", "import time; time.sleep(60)"], stdout=subprocess.PIPE, text=True)
out = smoke.lines_of(child.stdout)
start = time.time(); deadline = start + 2
try:
    while True:
        try:
            line = out.get(timeout=max(deadline - time.time(), 0))
        except queue.Empty:
            raise SystemExit("FAIL: nothing within 2 s")
except SystemExit as e:
    print("got:", e, "after", round(time.time() - start, 1), "s")
finally:
    child.kill(); child.wait()
try:
    smoke.run(sys.executable, "-c", "import time; time.sleep(5)", timeout=1)
except SystemExit as e:
    print("run:", e)

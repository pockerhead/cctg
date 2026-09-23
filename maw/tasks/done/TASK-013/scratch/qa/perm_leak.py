"""QA TASK-013: permission ids answered in the terminal never close in the agent.

Runs the real `cctg agent` against a minimal fake hub that registers it and
never sends verdicts (every prompt was answered at the terminal, which Claude
Code does not report to the channel server). Sends N distinct
permission_request notifications and counts how many reach the hub.
Also checks stdout purity and that the secret does not leak.

Usage: python perm_leak.py <cctg.exe> [N]
"""
import json, os, socket, subprocess, sys, tempfile, threading, time

EXE = sys.argv[1]
N = int(sys.argv[2]) if len(sys.argv) > 2 else 70
SECRET = "qa-synthetic-secret-0123456789"
ALPHA = "abcdefghijkmnopqrstuvwxyz"

def rid(n):
    out = []
    for _ in range(5):
        out.append(ALPHA[n % 25]); n //= 25
    return "".join(reversed(out))

srv = socket.socket(); srv.bind(("127.0.0.1", 0)); srv.listen()
port = srv.getsockname()[1]
got = []
registered = threading.Event()

def hub():
    conn, _ = srv.accept()
    f = conn.makefile("rb")
    for raw in f:
        msg = json.loads(raw)
        if msg.get("type") == "register":
            conn.sendall(b'{"v":1,"type":"registered"}\n'); registered.set()
        elif msg.get("type") == "permission_request":
            got.append(msg["request_id"])
threading.Thread(target=hub, daemon=True).start()

home = tempfile.mkdtemp(prefix="cctg_qa_home_")
env = {k: v for k, v in os.environ.items() if not k.startswith("CLAUDE") and not k.startswith("CCTG")}
env.update(USERPROFILE=home, HOME=home, CCTG_HUB_SECRET=SECRET,
           CCTG_HUB_AGENT_ADDR="127.0.0.1:%d" % port, CCTG_HOST="qa",
           CLAUDE_CODE_SESSION_ID="5e551017-0000-4000-8000-00000000qa01",
           CLAUDE_CODE_ENTRYPOINT="cli", RUST_LOG="trace")
p = subprocess.Popen([EXE, "agent"], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                     stderr=subprocess.PIPE, env=env)
def w(obj): p.stdin.write((json.dumps(obj) + "\n").encode()); p.stdin.flush()
w({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2025-11-25"}})
w({"jsonrpc": "2.0", "method": "notifications/initialized"})
assert registered.wait(10), "agent never registered"
for n in range(N):
    w({"jsonrpc": "2.0", "method": "notifications/claude/channel/permission_request",
       "params": {"request_id": rid(n), "tool_name": "Bash", "description": "d%d" % n,
                  "input_preview": "{}"}})
    time.sleep(0.01)
time.sleep(1.5)
p.stdin.close()
out, err = p.communicate(timeout=10)
print("sent", N, "distinct permission requests (no verdicts: answered at the terminal)")
print("relayed to hub:", len(got))
print("not relayed:", [rid(n) for n in range(N) if rid(n) not in got])
print("exit code:", p.returncode)
lines = [l for l in out.splitlines() if l.strip()]
print("stdout lines:", len(lines), "all json:", all(json.loads(l) is not None for l in lines))
print("secret in stdout/stderr:", SECRET.encode() in out, SECRET.encode() in err)
print("stderr tail:", err.decode(errors="replace").strip().splitlines()[-3:])

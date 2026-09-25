"""Container check of TASK-035, driven from the Windows host.

    python run_docker_e2e.py <tree to build> <client cctg.exe> [build id]

Builds the hub image from <tree> with CCTG_BUILD_ID=<build id>, starts it
with a fake Bot API (compose.test.yml), and checks with real client
processes (the given cctg.exe, built with the same CCTG_BUILD_ID):
  1. the image: `--version` carries the build id, runs as uid 10001, no
     env file or token inside, healthcheck healthy;
  2. a pinned `cctg hook SessionStart` from the host creates the topic;
     a hook with another pin does not;
  3. a pinned `cctg agent` registers over TLS and gets a topic message;
     the hub does not call it another build (same commit, other OS);
  4. an agent that says another build is logged as such, once;
  5. the hub logs its pin and never the secret or the token;
  6. `docker compose down` stops it cleanly (registry written).
Test values only; nothing here is a real secret. Exits non-zero on failure.
"""
import json
import os
import shutil
import socket
import ssl
import subprocess
import sys
import tempfile
import time
import urllib.request
from pathlib import Path

HERE = Path(__file__).resolve().parent
PROJECT = "cctg035"
IMAGE = "cctg:035-local"
SECRET = "docker-e2e-secret-0123456789abcdef"
TOKEN = "123456:docker-e2e-token-value"
SESSION = "5e550000-0000-4000-8000-0000000d0035"
AGENT_PORT, HOOK_PORT, CONTROL_PORT = 47391, 47392, 18081


def run(*args, check=True, **kwargs):
    print("$", " ".join(str(a) for a in args), flush=True)
    return subprocess.run([str(a) for a in args], check=check, text=True,
                          capture_output=True, **kwargs)


def compose(*args, check=True):
    return run("docker", "compose", "-p", PROJECT, "-f", HERE / "compose.test.yml", *args, check=check)


def control(path, body=None):
    data = None if body is None else json.dumps(body).encode()
    req = urllib.request.Request(f"http://127.0.0.1:{CONTROL_PORT}{path}", data=data,
                                 method="POST" if data else "GET")
    with urllib.request.urlopen(req, timeout=5) as resp:
        return json.loads(resp.read())


def wait(what, check, timeout=90):
    deadline = time.time() + timeout
    while time.time() < deadline:
        try:
            if check():
                return
        except Exception:
            pass
        time.sleep(0.5)
    raise SystemExit(f"FAIL: timed out waiting for {what}")


def client_env(home):
    env = {k: v for k, v in os.environ.items()
           if not (k.startswith("CCTG_") or k.startswith("CLAUDE"))}
    env["USERPROFILE"] = str(home)
    env["HOME"] = str(home)
    return env


def device(root, name, pin):
    home = root / name
    (home / ".cctg").mkdir(parents=True)
    lines = [f"CCTG_HUB_SECRET={SECRET}", f"CCTG_HUB_AGENT_ADDR=127.0.0.1:{AGENT_PORT}",
             f"CCTG_HUB_HOOK_ADDR=127.0.0.1:{HOOK_PORT}", "CCTG_HOST=box",
             f"CCTG_HUB_CERT_SHA256={pin}"]
    (home / ".cctg" / "device.env").write_text("\n".join(lines) + "\n", encoding="utf-8")
    return home


def hook(exe, home, work):
    payload = json.dumps({"session_id": SESSION, "cwd": str(work),
                          "transcript_path": str(work / f"{SESSION}.jsonl"),
                          "hook_event_name": "SessionStart", "source": "startup"})
    out = subprocess.run([str(exe), "hook", "SessionStart"], input=payload, text=True,
                         capture_output=True, env=client_env(home), cwd=work, timeout=30)
    assert out.returncode == 0 and out.stdout == "", out
    return out.stderr


def fingerprint(cert):
    out = run("openssl", "x509", "-in", cert, "-noout", "-fingerprint", "-sha256").stdout
    return out.strip().split("=", 1)[1]


def raw_register(build):
    """An agent over TLS (no pin check: test only) announcing `build`."""
    ctx = ssl.create_default_context()
    ctx.check_hostname = False
    ctx.verify_mode = ssl.CERT_NONE
    with socket.create_connection(("127.0.0.1", AGENT_PORT), timeout=10) as tcp:
        with ctx.wrap_socket(tcp, server_hostname="localhost") as tls:
            reg = {"v": 1, "type": "register", "session_id": "5e550000-0000-4000-8000-0000000d0036",
                   "host": "box", "cwd": "/w", "client": {"version": "0.1.0", "build": build,
                                                          "self_update": False}}
            tls.sendall((json.dumps({"v": 1, "type": "hello", "secret": SECRET}) + "\n"
                         + json.dumps(reg) + "\n").encode())
            answer = tls.recv(4096).decode()
            assert "registered" in answer, answer
            time.sleep(1)


def main():
    tree, exe = Path(sys.argv[1]), Path(sys.argv[2])
    build_id = sys.argv[3] if len(sys.argv) > 3 else "0" * 37 + "035"
    tls = HERE / "tls"
    shutil.rmtree(tls, ignore_errors=True)
    tls.mkdir()
    run("openssl", "req", "-x509", "-newkey", "ec", "-pkeyopt", "ec_paramgen_curve:prime256v1",
        "-nodes", "-days", "30", "-subj", "/CN=cctg-hub", "-keyout", tls / "key.pem",
        "-out", tls / "cert.pem")
    pin = fingerprint(tls / "cert.pem")
    (HERE / "test.env").write_text(
        f"CCTG_BOT_TOKEN={TOKEN}\nCCTG_CHAT_ID=-1000000000001\n"
        f"CCTG_ALLOWED_USER_IDS=1001\nCCTG_HUB_SECRET={SECRET}\n", encoding="utf-8")

    # 1. The image.
    run("docker", "build", "--build-arg", f"CCTG_BUILD_ID={build_id}", "-t", IMAGE, tree,
        stdout=None, capture_output=False)
    version = run("docker", "run", "--rm", IMAGE, "--version").stdout
    assert build_id in version, version
    assert run("docker", "run", "--rm", "--entrypoint", "id", IMAGE, "-u").stdout.strip() == "10001"
    found = run("docker", "run", "--rm", "--entrypoint", "sh", IMAGE, "-c",
                "find / -xdev \\( -name '.env' -o -name '*.env' -o -name 'registry.json' \\) "
                "-not -path '/proc/*' 2>/dev/null; true").stdout.strip()
    assert found == "", found
    inspect = run("docker", "image", "inspect", IMAGE).stdout
    history = run("docker", "history", "--no-trunc", IMAGE).stdout
    for text in (inspect, history):
        assert TOKEN not in text and SECRET not in text
    print("image ok:", version.strip(), flush=True)

    compose("down", "-v", check=False)
    compose("up", "-d")
    try:
        cid = compose("ps", "-q", "hub").stdout.strip()
        wait("a healthy hub", lambda: run("docker", "inspect", "--format",
                                          "{{.State.Health.Status}}", cid).stdout.strip() == "healthy")
        wait("polling", lambda: control("/control/calls").get("getUpdates", 0) > 0)

        root = Path(tempfile.mkdtemp(prefix="cctg-035-docker-"))
        work = root / "work"
        work.mkdir()
        # 2. Hooks: another pin, then the right one.
        other = root / "other"
        other.mkdir()
        run("openssl", "req", "-x509", "-newkey", "ec", "-pkeyopt", "ec_paramgen_curve:prime256v1",
            "-nodes", "-days", "1", "-subj", "/CN=x", "-keyout", other / "k.pem", "-out", other / "c.pem")
        stderr = hook(exe, device(root, "wrong", fingerprint(other / "c.pem")), work)
        assert "not delivered" in stderr, stderr
        time.sleep(1)
        assert control("/control/calls").get("topics", 0) == 0
        good = device(root, "good", pin)
        stderr = hook(exe, good, work)
        assert stderr == "", stderr
        wait("the topic", lambda: control("/control/calls").get("topics", 0) == 1)
        print("hooks ok", flush=True)

        # 3. The agent over TLS gets a topic message.
        agent = subprocess.Popen([str(exe), "agent"], cwd=work, stdin=subprocess.PIPE,
                                 stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True,
                                 env=dict(client_env(good), CLAUDE_CODE_SESSION_ID=SESSION,
                                          CLAUDE_CONFIG_DIR=str(good / "claude")))
        try:
            agent.stdin.write('{"jsonrpc":"2.0","id":1,"method":"initialize","params":'
                              '{"protocolVersion":"2025-11-25","capabilities":{}}}\n'
                              '{"jsonrpc":"2.0","method":"notifications/initialized"}\n')
            agent.stdin.flush()
            wait("the agent's registration",
                 lambda: "agent registered" in compose("logs", "hub").stdout)
            control("/control/push", {"thread_id": 101, "text": "over-tls-from-docker"})
            deadline = time.time() + 30
            got = False
            while time.time() < deadline and not got:
                line = agent.stdout.readline()
                got = "notifications/claude/channel" in line and "over-tls-from-docker" in line
            assert got, "the message did not reach the agent"
            print("agent ok", flush=True)
            logs = compose("logs", "hub").stdout
            assert "agent runs another cctg build" not in logs, "same commit called another build"

            # 4. Another build is named once.
            raw_register("f" * 40)
            logs = compose("logs", "hub").stdout
            assert logs.count("agent runs another cctg build") == 1, logs
        finally:
            agent.kill()
            agent.wait()

        # 5. Logs.
        logs = compose("logs", "hub").stdout
        assert "TLS certificate loaded" in logs and pin in logs, logs
        assert SECRET not in logs and TOKEN not in logs and "docker-e2e-token" not in logs
        print("logs ok", flush=True)
    finally:
        # 6. Stop.
        stop = compose("stop", "-t", "30", "hub", check=False)
        logs = compose("logs", "hub", check=False).stdout
        compose("down", "-v", check=False)
        print(logs[-2000:], flush=True)
        shutil.rmtree(tls, ignore_errors=True)
        (HERE / "test.env").unlink(missing_ok=True)
    assert "hub stopped" in logs, "graceful stop"
    print("PASS", flush=True)


if __name__ == "__main__":
    main()

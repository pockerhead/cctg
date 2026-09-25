"""CI container smoke of the hub image (TASK-035). Linux runner, Docker.

    python3 smoke.py <image> <client cctg> <build id>

<image> is the hub image built with CCTG_BUILD_ID=<build id>; <client
cctg> is a client binary built separately (other bytes) from the same
commit with the same CCTG_BUILD_ID. Starts the image with a fake Bot API
(compose.yml next to this file) and checks, with real client processes:
  1. the image: `--version` carries the build id, runs as uid 10001, holds
     no env file or registry, and neither the token nor the secret shows in
     its metadata or history;
  2. the healthcheck turns healthy and the hub polls;
  3. a hook pinned to another certificate creates nothing; the pinned hook
     creates the session's topic;
  4. the pinned agent registers over TLS and gets a topic message; the hub
     does not call it another build (one commit, other binary);
  5. an agent that says another build is logged as such, once;
  6. the hub logs its pin and never the secret or the token;
  7. `docker compose stop` stops it cleanly.
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
PROJECT = "cctgsmoke"
SECRET = "smoke-secret-0123456789abcdef"
TOKEN = "123456:smoke-token-value"
SESSION = "5e550000-0000-4000-8000-0000000d0035"
AGENT_PORT, HOOK_PORT, CONTROL_PORT = 47391, 47392, 18081


def run(*args, check=True, **kwargs):
    print("$", " ".join(str(a) for a in args), flush=True)
    return subprocess.run([str(a) for a in args], check=check, text=True,
                          capture_output=True, **kwargs)


def compose(image, *args, check=True):
    env = dict(os.environ, CCTG_SMOKE_IMAGE=image)
    return run("docker", "compose", "-p", PROJECT, "-f", HERE / "compose.yml", *args,
               check=check, env=env)


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
    env["HOME"] = str(home)
    env["USERPROFILE"] = str(home)
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


def new_cert(directory, cn, days):
    directory.mkdir(parents=True, exist_ok=True)
    run("openssl", "req", "-x509", "-newkey", "ec", "-pkeyopt", "ec_paramgen_curve:prime256v1",
        "-nodes", "-days", str(days), "-subj", f"/CN={cn}",
        "-keyout", directory / "key.pem", "-out", directory / "cert.pem")
    # A test key: readable by the image's uid 10001 through the bind mount.
    os.chmod(directory / "key.pem", 0o644)
    out = run("openssl", "x509", "-in", directory / "cert.pem", "-noout",
              "-fingerprint", "-sha256").stdout
    return out.strip().split("=", 1)[1]


def raw_register(build):
    """An agent over TLS (no pin check: test only) announcing `build`."""
    ctx = ssl.create_default_context()
    ctx.check_hostname = False
    ctx.verify_mode = ssl.CERT_NONE
    with socket.create_connection(("127.0.0.1", AGENT_PORT), timeout=10) as tcp:
        with ctx.wrap_socket(tcp, server_hostname="localhost") as tls:
            reg = {"v": 1, "type": "register", "session_id": "5e550000-0000-4000-8000-0000000d0036",
                   "host": "box", "cwd": "/w",
                   "client": {"version": "0.1.0", "build": build, "self_update": False}}
            tls.sendall((json.dumps({"v": 1, "type": "hello", "secret": SECRET}) + "\n"
                         + json.dumps(reg) + "\n").encode())
            answer = tls.recv(4096).decode()
            assert "registered" in answer, answer
            time.sleep(1)


def main():
    image, exe, build_id = sys.argv[1], Path(sys.argv[2]).resolve(), sys.argv[3]
    tls = HERE / "tls"
    shutil.rmtree(tls, ignore_errors=True)
    pin = new_cert(tls, "cctg-hub", 30)
    (HERE / "smoke.env").write_text(
        f"CCTG_BOT_TOKEN={TOKEN}\nCCTG_CHAT_ID=-1000000000001\n"
        f"CCTG_ALLOWED_USER_IDS=1001\nCCTG_HUB_SECRET={SECRET}\n", encoding="utf-8")

    # 1. The image.
    version = run("docker", "run", "--rm", image, "--version").stdout
    assert build_id in version, version
    assert build_id in run(exe, "--version").stdout, "client of the same build id"
    assert run("docker", "run", "--rm", "--entrypoint", "id", image, "-u").stdout.strip() == "10001"
    found = run("docker", "run", "--rm", "--entrypoint", "sh", image, "-c",
                "find / -xdev \\( -name '.env' -o -name '*.env' -o -name 'registry.json' \\) "
                "-not -path '/proc/*' 2>/dev/null; true").stdout.strip()
    assert found == "", found
    for text in (run("docker", "image", "inspect", image).stdout,
                 run("docker", "history", "--no-trunc", image).stdout):
        assert TOKEN not in text and SECRET not in text
    print("image ok:", version.strip(), flush=True)

    compose(image, "down", "-v", check=False)
    compose(image, "up", "-d")
    logs = ""
    try:
        # 2. Healthy and polling.
        cid = compose(image, "ps", "-q", "hub").stdout.strip()
        wait("a healthy hub", lambda: run("docker", "inspect", "--format",
                                          "{{.State.Health.Status}}", cid).stdout.strip() == "healthy")
        wait("polling", lambda: control("/control/calls").get("getUpdates", 0) > 0)

        root = Path(tempfile.mkdtemp(prefix="cctg-smoke-"))
        work = root / "work"
        work.mkdir()
        # 3. Hooks: another pin, then the right one.
        other = new_cert(root / "other", "x", 1)
        stderr = hook(exe, device(root, "wrong", other), work)
        assert "not delivered" in stderr, stderr
        time.sleep(1)
        assert control("/control/calls").get("topics", 0) == 0
        good = device(root, "good", pin)
        stderr = hook(exe, good, work)
        assert stderr == "", stderr
        wait("the topic", lambda: control("/control/calls").get("topics", 0) == 1)
        print("hooks ok", flush=True)

        # 4. The agent over TLS gets a topic message.
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
                 lambda: "agent registered" in compose(image, "logs", "hub").stdout)
            control("/control/push", {"thread_id": 101, "text": "over-tls-from-docker"})
            deadline = time.time() + 30
            got = False
            while time.time() < deadline and not got:
                line = agent.stdout.readline()
                got = "notifications/claude/channel" in line and "over-tls-from-docker" in line
            assert got, "the message did not reach the agent"
            print("agent ok", flush=True)
            logs = compose(image, "logs", "hub").stdout
            assert "agent runs another cctg build" not in logs, "same commit called another build"

            # 5. Another build is named once.
            raw_register("f" * 40)
            logs = compose(image, "logs", "hub").stdout
            assert logs.count("agent runs another cctg build") == 1, logs
        finally:
            agent.kill()
            agent.wait()

        # 6. Logs.
        logs = compose(image, "logs", "hub").stdout
        assert "TLS certificate loaded" in logs and pin in logs, logs
        assert SECRET not in logs and TOKEN not in logs, "a secret in the logs"
        print("logs ok", flush=True)
    finally:
        # 7. Stop.
        compose(image, "stop", "-t", "30", "hub", check=False)
        logs = compose(image, "logs", "hub", check=False).stdout
        compose(image, "down", "-v", check=False)
        print(logs[-3000:], flush=True)
        shutil.rmtree(tls, ignore_errors=True)
        (HERE / "smoke.env").unlink(missing_ok=True)
    assert "hub stopped" in logs, "graceful stop"
    print("PASS", flush=True)


if __name__ == "__main__":
    main()

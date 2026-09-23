"""TASK-013 live check: a stand-in for `cctg hub` on the agent link (wire v1).

Accepts agents, answers hello/register with `registered`, logs every line it
gets (secret redacted) to a JSONL file, answers permission requests with
`allow`, and sends each inbound from --inbound at its offset (seconds after
the first registration) to every registered agent.

Usage: python fake_hub.py <port> <secret> <log> [--inbound SEC:TEXT ...]
"""
import json
import socket
import sys
import threading
import time

PORT, SECRET, LOG = int(sys.argv[1]), sys.argv[2], sys.argv[3]
INBOUND = []
args = sys.argv[4:]
while args:
    flag = args.pop(0)
    if flag == "--inbound":
        sec, text = args.pop(0).split(":", 1)
        INBOUND.append((float(sec), text))

lock = threading.Lock()
agents = []
first_registration = []


def log(kind, **fields):
    rec = {"t": time.strftime("%H:%M:%S"), "kind": kind}
    rec.update(fields)
    with lock:
        with open(LOG, "a", encoding="utf-8") as fh:
            fh.write(json.dumps(rec, ensure_ascii=False) + "\n")
    print(json.dumps(rec, ensure_ascii=False), flush=True)


def send(conn, msg):
    msg = dict(msg, v=1)
    conn.sendall((json.dumps(msg, ensure_ascii=False) + "\n").encode("utf-8"))


def serve(conn, peer):
    reader = conn.makefile("rb")
    registered = False
    for raw in reader:
        try:
            msg = json.loads(raw)
        except ValueError:
            log("bad_line")
            continue
        if msg.get("type") == "hello":
            ok = msg.get("secret") == SECRET
            log("hello", ok=ok)
            if not ok:
                send(conn, {"type": "rejected", "reason": "auth"})
                return
        elif msg.get("type") == "register":
            log("register", register=msg)
            send(conn, {"type": "registered"})
            registered = True
            with lock:
                agents.append(conn)
                if not first_registration:
                    first_registration.append(time.time())
        elif msg.get("type") == "permission_request":
            log("permission_request", msg=msg)
            send(conn, {"type": "permission_verdict", "request_id": msg["request_id"],
                        "behavior": "allow"})
            log("verdict_sent", request_id=msg["request_id"])
        else:
            log("agent_msg", msg=msg)
    log("disconnected", registered=registered)
    with lock:
        if conn in agents:
            agents.remove(conn)


def inbound_schedule():
    while not first_registration:
        time.sleep(0.2)
    start = first_registration[0]
    for sec, text in sorted(INBOUND):
        wait = start + sec - time.time()
        if wait > 0:
            time.sleep(wait)
        with lock:
            targets = list(agents)
        for conn in targets:
            try:
                send(conn, {"type": "inbound", "content": text,
                            "meta": {"chat_id": "-1001", "bad-key": "dropped"}})
            except OSError:
                pass
        log("inbound_sent", text=text, agents=len(targets))


def main():
    srv = socket.socket()
    srv.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    srv.bind(("127.0.0.1", PORT))
    srv.listen()
    log("listening", port=PORT)
    threading.Thread(target=inbound_schedule, daemon=True).start()
    while True:
        conn, peer = srv.accept()
        threading.Thread(target=serve, args=(conn, peer), daemon=True).start()


if __name__ == "__main__":
    main()

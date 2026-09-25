"""Fake Telegram Bot API for the container check of TASK-035 (stdlib only).

Serves the Bot API methods the hub calls at start and while polling, on
127.0.0.1:8081 inside the network namespace the hub shares with it. A test
drives it over the same port:
  POST /control/push   {"thread_id": 101, "text": "hi"}  -> a topic message update
  GET  /control/calls                                     -> {"getMe": 1, ...}
No real token: any /bot<anything>/<method> path is answered.
"""
import json
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

CHAT = -1000000000001
USER = 1001
ICONS = ["5312016608254762256", "5408906741125490282", "5377316857231450742", "5357121491508928442"]

lock = threading.Condition()
calls = {}
updates = []
counters = {"topics": 0, "messages": 0}


def answer(method, body):
    ok = lambda result: {"ok": True, "result": result}
    if method == "getMe":
        return ok({"id": 3003, "is_bot": True, "first_name": "b", "username": "fake_bot"})
    if method == "getChatMember":
        return ok({"status": "administrator", "can_manage_topics": True, "can_delete_messages": True})
    if method == "getForumTopicIconStickers":
        return ok([{"custom_emoji_id": i, "emoji": "x"} for i in ICONS])
    if method == "getUpdates":
        offset = int(body.get("offset") or 0)
        deadline = time.time() + 1
        with lock:
            while True:
                found = [u for u in updates if u["update_id"] >= offset]
                if found or time.time() >= deadline:
                    return ok(found)
                lock.wait(timeout=max(0.0, deadline - time.time()))
    if method == "createForumTopic":
        with lock:
            counters["topics"] += 1
            return ok({"message_thread_id": 100 + counters["topics"], "name": body.get("name"), "icon_color": 0})
    if method in ("sendMessage", "editMessageText"):
        with lock:
            counters["messages"] += 1
            return ok({"message_id": counters["messages"], "message_thread_id": body.get("message_thread_id"),
                       "date": 1, "chat": {"id": CHAT}})
    return ok(True)


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *args):
        pass

    def _send(self, payload):
        data = json.dumps(payload).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(data)))
        self.send_header("Connection", "close")
        self.end_headers()
        self.wfile.write(data)

    def _body(self):
        length = int(self.headers.get("Content-Length") or 0)
        raw = self.rfile.read(length) if length else b""
        try:
            return json.loads(raw or b"{}")
        except ValueError:
            return {}

    def do_GET(self):
        if self.path == "/control/calls":
            with lock:
                return self._send(dict(calls, **counters))
        self.do_POST()

    def do_POST(self):
        body = self._body()
        if self.path == "/control/push":
            with lock:
                update_id = len(updates) + 1
                updates.append({"update_id": update_id, "message": {
                    "message_id": 5000 + update_id, "message_thread_id": body["thread_id"],
                    "is_topic_message": True, "date": 1, "text": body["text"],
                    "from": {"id": USER, "is_bot": False, "first_name": "u"},
                    "chat": {"id": CHAT, "type": "supergroup", "is_forum": True}}})
                lock.notify_all()
            return self._send({"ok": True})
        method = self.path.rsplit("/", 1)[-1].split("?", 1)[0]
        with lock:
            calls[method] = calls.get(method, 0) + 1
        self._send(answer(method, body))


if __name__ == "__main__":
    ThreadingHTTPServer(("0.0.0.0", 8081), Handler).serve_forever()

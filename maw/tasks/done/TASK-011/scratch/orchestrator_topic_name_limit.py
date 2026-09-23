"""Probe how Telegram counts the 128 limit of a forum topic name. Creates and deletes
throwaway topics. Prints only ok/description, never the token or chat id."""
import json, pathlib, urllib.request, urllib.parse

env = {}
for line in pathlib.Path("C:/Users/user/dev/cctg/.env").read_text(encoding="utf-8").splitlines():
    if "=" in line:
        k, v = line.split("=", 1)
        env[k.strip()] = v.strip()
base = f"https://api.telegram.org/bot{env['CCTG_BOT_TOKEN']}/"
chat = env["CCTG_CHAT_ID"]


def call(method, **params):
    data = urllib.parse.urlencode(params).encode("utf-8")
    try:
        with urllib.request.urlopen(base + method, data=data, timeout=20) as r:
            return json.loads(r.read())
    except urllib.error.HTTPError as e:
        return json.loads(e.read())


cases = {
    "cyr128 (128 chars, 256 utf8 bytes)": "Ж" * 128,
    "cyr129": "Ж" * 129,
    "emoji64 (64 astral = 128 utf16 units)": "\U0001F600" * 64,
    "emoji65 (130 utf16 units)": "\U0001F600" * 65,
    "emoji128 (128 code points, 256 utf16)": "\U0001F600" * 128,
}
for label, name in cases.items():
    r = call("createForumTopic", chat_id=chat, name=name)
    if r.get("ok"):
        tid = r["result"]["message_thread_id"]
        d = call("deleteForumTopic", chat_id=chat, message_thread_id=tid)
        print(f"{label}: ACCEPTED (deleted={d.get('ok')})")
    else:
        print(f"{label}: REJECTED {r.get('description')}")

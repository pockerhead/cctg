# Read-only Bot API shape probe. Prints only keys and JSON types; never the token,
# chat id, user ids or names. Errors are reported without the URL.
import json, os, sys, urllib.request, urllib.error

REPO = r"C:/Users/user/dev/cctg"
env = {}
for line in open(os.path.join(REPO, ".env"), encoding="utf-8"):
    line = line.strip()
    if line and not line.startswith("#") and "=" in line:
        k, v = line.split("=", 1)
        env[k.strip()] = v.strip().strip('"')
TOKEN = env["CCTG_BOT_TOKEN"]
CHAT = env["CCTG_CHAT_ID"]

def call(method, params=None):
    data = json.dumps(params or {}).encode()
    req = urllib.request.Request(f"https://api.telegram.org/bot{TOKEN}/{method}", data=data,
                                 headers={"Content-Type": "application/json"})
    try:
        with urllib.request.urlopen(req, timeout=20) as r:
            return r.status, json.loads(r.read())
    except urllib.error.HTTPError as e:
        return e.code, json.loads(e.read())
    except Exception as e:
        return None, {"transport_error": type(e).__name__}

def shape(v):
    if isinstance(v, dict):
        return {k: shape(x) for k, x in v.items()}
    if isinstance(v, list):
        return [shape(v[0])] if v else []
    if isinstance(v, bool):
        return f"bool:{v}"
    return type(v).__name__

out = {}
st, me = call("getMe"); out["getMe"] = {"http": st, "body": shape(me)}
bot_id = me.get("result", {}).get("id")
st, cm = call("getChatMember", {"chat_id": int(CHAT), "user_id": bot_id}); out["getChatMember(bot)"] = {"http": st, "body": shape(cm)}
st, st_ = call("getForumTopicIconStickers"); out["getForumTopicIconStickers"] = {"http": st, "count": len(st_.get("result", [])), "body": shape(st_)}
st, wh = call("getWebhookInfo"); out["getWebhookInfo"] = {"http": st, "body": shape(wh),
    "pending_update_count": wh.get("result", {}).get("pending_update_count"), "url_set": bool(wh.get("result", {}).get("url"))}
# Error envelope shape: method that does not exist, and a bad parameter (no side effects).
st, e1 = call("getChatMember", {"chat_id": int(CHAT), "user_id": 1}); out["getChatMember(bad user)"] = {"http": st, "body": shape(e1), "description": e1.get("description")}
st, e2 = call("noSuchMethod"); out["noSuchMethod"] = {"http": st, "body": shape(e2), "description": e2.get("description")}
text = json.dumps(out, indent=1, ensure_ascii=False)
for secret in [TOKEN, CHAT, CHAT.replace("-100", ""), str(bot_id), TOKEN.split(":")[0]]:
    assert secret not in text, "secret leaked into output"
print(text)

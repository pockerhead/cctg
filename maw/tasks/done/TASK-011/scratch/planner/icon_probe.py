# Read-only: getForumTopicIconStickers. The token is read from the repo .env
# and never printed; only emoji and custom_emoji_id are saved.
import json, os, urllib.request
ROOT = os.path.join(os.path.dirname(os.path.abspath(__file__)), *(['..'] * 6))
token = None
with open(os.path.join(ROOT, '.env'), encoding='utf-8') as f:
    for line in f:
        if line.startswith('CCTG_BOT_TOKEN='):
            token = line.split('=', 1)[1].strip().strip('"')
req = urllib.request.Request('https://api.telegram.org/bot%s/getForumTopicIconStickers' % token,
                             data=b'{}', headers={'Content-Type': 'application/json'})
body = json.load(urllib.request.urlopen(req, timeout=20))
out = [{'emoji': s.get('emoji'), 'custom_emoji_id': s.get('custom_emoji_id')} for s in body.get('result', [])]
path = os.path.join(os.path.dirname(os.path.abspath(__file__)), 'icon_stickers.json')
with open(path, 'w', encoding='utf-8', newline='\n') as f:
    json.dump({'ok': body.get('ok'), 'count': len(out), 'stickers': out}, f, ensure_ascii=False, indent=1)
print('ok', body.get('ok'), 'count', len(out))

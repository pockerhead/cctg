import json,glob,os,re
for f in glob.glob(os.path.expanduser('~/.claude/projects/**/*.jsonl'),recursive=True):
    for l in open(f,encoding='utf-8',errors='replace'):
        if '<command-message>' not in l: continue
        try: r=json.loads(l)
        except: continue
        ct=(r.get('message') or {}).get('content')
        if r.get('type')=='user' and isinstance(ct,str) and ct.startswith('<command-message>') and not r.get('isMeta'):
            print(os.path.basename(f)[:8], r.get('version'), repr(ct[:300]))

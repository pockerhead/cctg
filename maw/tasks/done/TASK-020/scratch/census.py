import json,glob,os,collections,re
c=collections.Counter(); ex={}
for f in glob.glob(os.path.expanduser('~/.claude/projects/**/*.jsonl'),recursive=True):
    try: lines=open(f,encoding='utf-8',errors='replace').read().splitlines()
    except: continue
    for l in lines:
        if '<command-' not in l: continue
        try: r=json.loads(l)
        except: continue
        if r.get('type')!='user': continue
        m=r.get('message') or {}
        ct=m.get('content')
        texts=[ct] if isinstance(ct,str) else [b.get('text','') for b in (ct or []) if isinstance(b,dict) and b.get('type')=='text']
        for t in texts:
            if not t or '<command-' not in t: continue
            first=re.match(r'\s*<([a-z-]+)>',t)
            tags=tuple(re.findall(r'<(command-[a-z]+)>',t))
            key=(bool(r.get('isMeta')), bool(r.get('isSidechain')), first.group(1) if first else 'NOTAG', tags, isinstance(ct,str), t.startswith('<command-name>'))
            c[key]+=1
            ex.setdefault(key, t[:200])
for k,v in c.most_common(): print(v,k); print('   ',repr(ex[k]))

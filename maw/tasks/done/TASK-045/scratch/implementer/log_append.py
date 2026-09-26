# Appends implementer entries to the task log: BOM-free UTF-8, one object per
# line, real UTC time. Usage: python log_append.py <kind> <body> [ref ...]
import datetime, json, os, sys
here = os.path.dirname(os.path.abspath(__file__))
log = os.path.normpath(os.path.join(here, '..', '..', 'log.jsonl'))
kind, body, refs = sys.argv[1], sys.argv[2], sys.argv[3:]
assert kind in ('dead_end', 'decision') and body.strip()
entry = {
    'ts': datetime.datetime.now(datetime.timezone.utc).strftime('%Y-%m-%dT%H:%M:%SZ'),
    'stage': 'implementer', 'provider': 'claude', 'model': 'opus', 'effort': 'medium',
    'kind': kind, 'body': body, 'refs': refs,
}
with open(log, 'a', encoding='utf-8', newline='\n') as f:
    f.write(json.dumps(entry, ensure_ascii=False) + '\n')
print(entry['ts'])

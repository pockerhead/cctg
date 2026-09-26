"""TASK-057: print the input box rows of conhost_<tag>.json dumps with attribute runs."""
import json, sys
for tag in sys.argv[1:]:
    d = json.load(open('conhost_%s.json' % tag, encoding='utf-8'))
    print('==', tag, 'cursor', d['cursor'])
    rows = d['rows']
    idx = [i for i, r in enumerate(rows) if r['text'].startswith('─' * 20)]
    for r in rows[idx[-2]:idx[-1] + 1]:
        runs = []; prev = None
        for i, x in enumerate(r['attrs']):
            if x != prev: runs.append('%d:%s' % (i, x)); prev = x
        print(repr(r['text']), ' '.join(runs[:8]))

# Read-only survey of message.stop_reason in real transcripts of this machine.
# Prints only aggregate counts and anonymized shapes, never content.
import json, glob, os, collections, sys
root = os.path.expanduser('~/.claude/projects')
files = glob.glob(os.path.join(root, '*', '*.jsonl')) + glob.glob(os.path.join(root, '*', '*', 'subagents', '*.jsonl'))
by_type = collections.Counter()
same_msg_mismatch = 0
msgs = 0
last_text_sr = collections.Counter()      # stop_reason of the last assistant text before a real user prompt / EOF
tail_kind = collections.Counter()
synthetic = collections.Counter()
text_toolstop_followed_by_tooluse_same_msg = 0
text_toolstop_not = 0
endturn_multi_text = 0
for f in files:
    sub = 'subagents' in f
    recs = []
    for line in open(f, encoding='utf-8', errors='replace'):
        try: r = json.loads(line)
        except Exception: continue
        if not isinstance(r, dict): continue
        if r.get('type') not in ('user', 'assistant'): continue
        recs.append(r)
    per_msg = collections.defaultdict(list)
    for i, r in enumerate(recs):
        m = r.get('message') or {}
        if r['type'] == 'assistant':
            c = m.get('content')
            kinds = [b.get('type') for b in c] if isinstance(c, list) else ['<str>']
            sr = m.get('stop_reason', '<absent>')
            by_type[(('sub' if sub else 'main'), ','.join(kinds), str(sr))] += 1
            if m.get('model') == '<synthetic>' or r.get('isApiErrorMessage'):
                synthetic[(str(sr), ','.join(kinds))] += 1
            per_msg[m.get('id')].append((i, kinds, sr))
    for mid, rows in per_msg.items():
        msgs += 1
        if len(set(str(s) for _, _, s in rows)) > 1: same_msg_mismatch += 1
        for k, (i, kinds, sr) in enumerate(rows):
            if 'text' in kinds and sr == 'tool_use':
                if any('tool_use' in kk for _, kk, _ in rows[k+1:]): text_toolstop_followed_by_tooluse_same_msg += 1
                else: text_toolstop_not += 1
        if sum('text' in kk for _, kk, s in rows if s == 'end_turn') > 1: endturn_multi_text += 1
    # tail analysis
    for i in range(len(recs)-1, -1, -1):
        r = recs[i]
        if r['type'] == 'assistant':
            m = r.get('message') or {}
            c = m.get('content'); kinds = [b.get('type') for b in c] if isinstance(c, list) else ['<str>']
            tail_kind[(('sub' if sub else 'main'), ','.join(kinds), str(m.get('stop_reason', '<absent>')))] += 1
            break
        else:
            c = (r.get('message') or {}).get('content')
            if isinstance(c, list) and c and c[0].get('type') == 'tool_result':
                tail_kind[(('sub' if sub else 'main'), 'user:tool_result', '')] += 1; break
            tail_kind[(('sub' if sub else 'main'), 'user:prompt', '')] += 1; break
print('FILES', len(files), 'MESSAGES', msgs)
print('STOP_REASON_MISMATCH_WITHIN_MESSAGE_ID', same_msg_mismatch)
print('TEXT_TOOLUSE_STOP followed by tool_use in same msg:', text_toolstop_followed_by_tooluse_same_msg, 'not followed:', text_toolstop_not)
print('MESSAGES_WITH_>1_TEXT_RECORD_AT_END_TURN', endturn_multi_text)
print('--- assistant records by (scope, content kinds, stop_reason)')
for k, v in sorted(by_type.items(), key=lambda x: -x[1]): print(v, k)
print('--- synthetic/api-error assistant records (stop_reason, kinds)')
for k, v in synthetic.items(): print(v, k)
print('--- file tails')
for k, v in sorted(tail_kind.items(), key=lambda x: -x[1]): print(v, k)

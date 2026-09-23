# Builds crates/cctg/tests/fixtures/hook/*.json in the reference workspace from
# the redacted TASK-003 captures (home already replaced by "~").
import json, os
SRC = os.path.join(os.path.dirname(__file__), '..', '..', '..', '..', 'done', 'TASK-003', 'scratch')
OUT = os.path.join(os.path.dirname(__file__), 'ws', 'crates', 'cctg', 'tests', 'fixtures', 'hook')
os.makedirs(OUT, exist_ok=True)
def rec(name, i):
    return json.loads(open(os.path.join(SRC, name), encoding='utf-8').read().splitlines()[i])['stdin']
fx = {
  'session_start': rec('capture_E_interactive.jsonl', 0),
  'session_end': rec('capture_A_toplevel.jsonl', 2),
  'stop': rec('capture_A_toplevel.jsonl', 1),
  'subagent_start': rec('capture_C_mixed.jsonl', 7),
  'subagent_stop': rec('capture_C_mixed.jsonl', 8),
  'subagent_stop_internal': rec('capture_unknown.jsonl', 1),
  'pre_tool_use_handback': rec('capture_unknown.jsonl', 8),
  'post_tool_use_handback': rec('capture_unknown.jsonl', 9),
}
fx['stop']['last_assistant_message'] = 'Ok.'  # capture holds mojibake of a Russian word
fx['subagent_stop_internal']['last_assistant_message'] = 'Running a command'
fx['subagent_stop_internal']['background_tasks'] = []
# No UserPromptSubmit was captured in TASK-003: synthetic, shaped after the
# common fields of the captures plus the documented `prompt` field.
s = fx['session_end']
fx['user_prompt_submit'] = {
  'session_id': s['session_id'], 'transcript_path': s['transcript_path'], 'cwd': s['cwd'],
  'prompt_id': s['prompt_id'], 'permission_mode': 'default', 'hook_event_name': 'UserPromptSubmit',
  'prompt': 'private prompt text that must not reach the hub'}
for name, value in fx.items():
    with open(os.path.join(OUT, name + '.json'), 'w', encoding='utf-8', newline='\n') as f:
        f.write(json.dumps(value, ensure_ascii=False, indent=2) + '\n')
print(sorted(fx))

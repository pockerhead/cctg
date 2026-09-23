# One mutation at a time against reviewer2/ws: apply, run the named tests, restore.
# KILLED = the tests fail. M* = planner mutations re-targeted at the fixed code,
# N* = mutations for the reviewer-2 fixes.
import os
import subprocess

HERE = os.path.dirname(os.path.abspath(__file__))
WS = os.path.join(HERE, 'ws')
SRC = os.path.join(WS, 'crates', 'cctg', 'src', 'hub')
env = dict(os.environ, CARGO_TARGET_DIR=os.path.join(os.environ['TEMP'], 'cctg-task011-plan-target'))
LIB_HUB = ['--lib', '--', 'hub::']
M = [
    ('M1 no case-fold', 'registry.rs', 'path.to_lowercase()', 'path'),
    ('M2 every slot free', 'registry.rs',
     'None => true,\n            Some(session) => self.sessions.get(session).is_none_or(|entry| entry.ended),',
     'None => true,\n            Some(_) => true,'),
    ('M3 invalid ignores thread', 'registry.rs',
     'let slot = &mut self.slots[id.0];\n        if slot.topic_id == Some(thread_id) {\n            slot.busy = false;\n            slot.topic_id = None;',
     'let slot = &mut self.slots[id.0];\n        if true {\n            slot.busy = false;\n            slot.topic_id = None;'),
    ('M4 nested gets own slot', 'registry.rs', 'let slot = match &kind {\n            SessionKind::TopLevel => {',
     'let slot = match &SessionKind::TopLevel {\n            SessionKind::TopLevel => {'),
    ('M5 separator for first session', 'registry.rs', 'None => slot.current_session = Some(session.to_owned()),',
     'None => {\n                slot.pending_separator = Some(separator(session, resumed));\n                slot.current_session = Some(session.to_owned());\n            }'),
    ('M6 warn every delete failure', 'slots.rs', 'self.delete_warned = true;', 'self.delete_warned = false;', ['--test', 'slots_logs']),
    ('M7 not_modified is failure', 'slots.rs', 'telegram_error(delivery, &["topic_not_modified"])', 'false'),
    ('M8 no grace', 'slots.rs', 'let edits = Instant::now() >= self.grace_until;', 'let edits = true;'),
    ('M9 delete in any topic', 'slots.rs',
     'if !self.options.can_delete\n            || thread_id\n                .and_then(|t| self.registry.slot_by_topic(t))\n                .is_none()\n        {',
     'if !self.options.can_delete {'),
    ('M10 title never read', 'registry.rs', 'let wants_title = entry.title.is_none()', 'let wants_title = false && entry.title.is_none()'),
    ('M11 no pending agent bind', 'slots.rs', 'self.registry.agent_connected(session, conn);\n        }\n        if let Some((session, path))',
     'let _ = conn;\n        }\n        if let Some((session, path))'),
    ('M12 title cut ignores ordinal', 'registry.rs', 'let folder_room = MAX_TITLE - telegram_len(&head) - 1 - telegram_len(&suffix);',
     'let folder_room = MAX_TITLE - telegram_len(&head) - 1;'),
    # reviewer-2 fixes
    ('N2 unknown prompt hook adopts', 'registry.rs',
     'let Some(entry) = self.sessions.get_mut(session) else {\n                    return Followup::default();\n                };\n                entry.waiting = false;',
     'if !self.sessions.contains_key(session) {\n                    self.session_started(post, None, None, None);\n                }\n                let Some(entry) = self.sessions.get_mut(session) else {\n                    return Followup::default();\n                };\n                entry.waiting = false;'),
    ('N3a self parent guard never fires', 'registry.rs', '.map(String::as_str) == Some(session)\n        }) {',
     '.map(String::as_str) == Some("\\u{0}")\n        }) {'),
    ('N3b self parent is top-level', 'registry.rs', '            return SlotOrParent::Parent(None);\n        }\n        let kind',
     '            return SlotOrParent::Own(self.sessions[session].slot.expect("own"));\n        }\n        let kind'),
    ('N4 separator and edit together', 'registry.rs', '                    text,\n                });\n                continue;\n            }\n            if !edits {',
     '                    text,\n                });\n            }\n            if !edits {'),
    ('N5a late invalid releases busy', 'registry.rs',
     'let slot = &mut self.slots[id.0];\n        if slot.topic_id == Some(thread_id) {\n            slot.busy = false;\n            slot.topic_id = None;',
     'let slot = &mut self.slots[id.0];\n        slot.busy = false;\n        if slot.topic_id == Some(thread_id) {\n            slot.topic_id = None;'),
    ('N5b stale edit releases busy', 'registry.rs',
     '        let slot = &mut self.slots[id.0];\n        if slot.topic_id != Some(thread_id) {\n            return;\n        }\n        slot.busy = false;\n        slot.failed = None;\n        if let Some(name)',
     '        let slot = &mut self.slots[id.0];\n        slot.busy = false;\n        if slot.topic_id != Some(thread_id) {\n            return;\n        }\n        slot.failed = None;\n        if let Some(name)'),
    ('N6a separator taken at enqueue', 'registry.rs', 'slot.pending_separator.clone()', 'slot.pending_separator.take()'),
    ('N6b failed separator counts as sent', 'slots.rs',
     '"session separator not delivered; retrying later");\n                    }\n                    self.registry.topic_failed(slot, icons);',
     '"session separator not delivered; retrying later");\n                    }\n                    self.registry.topic_separated(slot, thread_id, &text);'),
    ('N7a missing preferred icon kept', 'registry.rs', 'if offered.contains(wanted) {', 'if true {'),
    ('N7b lookup error keeps defaults', 'mod.rs',
     'lookup.context("getForumTopicIconStickers failed; topic icons must come from it")?;',
     'match lookup {\n            Ok(stickers) => stickers,\n            Err(_) => return Ok(Icons::default()),\n        };'),
    ('N8 title from the head only', 'slots.rs', 'const TITLE_SCAN_BYTES: u64 = 256 * 1024 * 1024;', 'const TITLE_SCAN_BYTES: u64 = 4 * 1024 * 1024;'),
]
out = []
for entry in M:
    name, f, a, b = entry[:4]
    args = entry[4] if len(entry) > 4 else LIB_HUB
    path = os.path.join(SRC, f)
    orig = open(path, encoding='utf-8').read()
    if orig.count(a) != 1:
        out.append(f'{name}: PATTERN NOT FOUND ({orig.count(a)})')
        print(out[-1], flush=True)
        continue
    open(path, 'w', encoding='utf-8', newline='\n').write(orig.replace(a, b, 1))
    try:
        r = subprocess.run(['cargo', 'test', '-p', 'cctg', '--offline'] + args, cwd=WS, env=env,
                           capture_output=True, text=True, encoding='utf-8', errors='replace', timeout=900)
        if 'error[' in r.stderr or 'could not compile' in r.stderr:
            verdict = 'KILLED (compile error)'
        else:
            verdict = 'KILLED' if r.returncode != 0 else 'SURVIVED'
        failed = [l.split()[1] for l in r.stdout.splitlines() if l.startswith('test ') and l.endswith('FAILED')]
        out.append(f'{name}: {verdict} {failed}')
    finally:
        open(path, 'w', encoding='utf-8', newline='\n').write(orig)
    print(out[-1], flush=True)
open(os.path.join(HERE, 'mutations.out.txt'), 'w', encoding='utf-8', newline='\n').write('\n'.join(out) + '\n')

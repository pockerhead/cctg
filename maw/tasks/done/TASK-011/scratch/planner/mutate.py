# One mutation at a time: apply, run the named tests, restore. KILLED = tests fail.
import os, subprocess
HERE = os.path.dirname(os.path.abspath(__file__))
WS = os.path.join(HERE, 'ws')
HUB = os.path.join(WS, 'crates', 'cctg', 'src', 'hub')
env = dict(os.environ, CARGO_TARGET_DIR=os.path.join(os.environ['TEMP'], 'cctg-task011-plan-target'))
M = [
 ('M1 no case-fold', 'registry.rs', 'path.to_lowercase()', 'path', ['--lib', 'hub::registry']),
 ('M2 every slot free', 'registry.rs', 'None => true,\n            Some(session) => self.sessions.get(session).is_none_or(|entry| entry.ended),', 'None => true,\n            Some(_) => true,', ['--lib', 'hub::registry']),
 ('M3 invalid ignores thread', 'registry.rs', 'slot.busy = false;\n        if slot.topic_id == Some(thread_id) {', 'slot.busy = false;\n        if slot.topic_id.is_some() || slot.topic_id.is_none() {', ['--lib', 'hub::registry']),
 ('M4 nested gets own slot', 'registry.rs', 'let slot = match &kind {\n            SessionKind::TopLevel => {', 'let slot = match &SessionKind::TopLevel {\n            SessionKind::TopLevel => {', ['--lib', 'hub::registry']),
 ('M5 separator for first session', 'registry.rs', 'None => slot.current_session = Some(session.to_owned()),', 'None => {\n                slot.pending_separator = Some(separator(session, resumed));\n                slot.current_session = Some(session.to_owned());\n            }', ['--lib', 'hub::']),
 ('M6 warn every delete failure', 'slots.rs', 'self.delete_warned = true;', 'self.delete_warned = false;', ['--test', 'slots_logs']),
 ('M7 not_modified is failure', 'slots.rs', 'telegram_error(delivery, &["topic_not_modified"])', 'false', ['--lib', 'hub::slots']),
 ('M8 no grace', 'slots.rs', 'let edits = Instant::now() >= self.grace_until;', 'let edits = true;', ['--lib', 'hub::slots']),
 ('M9 delete in any topic', 'slots.rs', 'if !self.options.can_delete\n            || thread_id\n                .and_then(|t| self.registry.slot_by_topic(t))\n                .is_none()\n        {', 'if !self.options.can_delete {', ['--lib', 'hub::slots']),
 ('M10 title never read', 'registry.rs', 'let wants_title = entry.title.is_none()', 'let wants_title = false && entry.title.is_none()', ['--lib', 'hub::slots']),
 ('M11 no pending agent bind', 'slots.rs', '            self.registry.agent_connected(session, pending.conn);\n        }\n        if let Some((session, path))', '            let _ = pending;\n        }\n        if let Some((session, path))', ['--lib', 'hub::slots']),
 ('M12 title cut ignores ordinal', 'registry.rs', 'let folder_room = MAX_TITLE - telegram_len(&head) - 1 - telegram_len(&suffix);', 'let folder_room = MAX_TITLE - telegram_len(&head) - 1;', ['--lib', 'hub::registry']),
]
out = []
for name, f, a, b, args in M:
    path = os.path.join(HUB, f)
    orig = open(path, encoding='utf-8').read()
    if a not in orig:
        out.append(f'{name}: PATTERN NOT FOUND'); continue
    open(path, 'w', encoding='utf-8', newline='\n').write(orig.replace(a, b, 1))
    try:
        r = subprocess.run(['cargo', 'test', '-p', 'cctg', '--offline'] + args[:1] + ([args[1]] if args[0] == '--test' else ['--', args[1]]),
                           cwd=WS, env=env, capture_output=True, text=True, encoding='utf-8', errors='replace')
        out.append(f'{name}: {"KILLED" if r.returncode != 0 else "SURVIVED"}')
    finally:
        open(path, 'w', encoding='utf-8', newline='\n').write(orig)
    print(out[-1], flush=True)
open(os.path.join(HERE, 'mutations.out.txt'), 'w', encoding='utf-8', newline='\n').write('\n'.join(out) + '\n')

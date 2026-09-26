"""Adds the new `heartbeat` field to existing struct literals (TASK-049)."""
import re, sys
sys.path.insert(0, 'maw/tasks/in_progress/TASK-049/scratch')
from sub import Sub

# The hub now answers every registration with heartbeat: true.
f = Sub('crates/cctg/src/hub/ingress.rs')
n = f.s.count('HubMsg::Registered { files: true }')
f.s = f.s.replace('HubMsg::Registered { files: true }', 'HubMsg::Registered { files: true, heartbeat: true }')
print('ingress Registered', n)
f.save()

f = Sub('crates/cctg/src/agent.rs')
for old, new in [
    ('HubMsg::Registered { files: true }', 'HubMsg::Registered { files: true, heartbeat: false }'),
    ('HubMsg::Registered { files: false }', 'HubMsg::Registered { files: false, heartbeat: false }'),
    ('HubMsg::Registered { files }', 'HubMsg::Registered { files, heartbeat: false }'),
]:
    print('agent', old, f.s.count(old))
    f.s = f.s.replace(old, new)
f.save()

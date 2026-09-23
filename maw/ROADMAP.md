# Roadmap graph (derived from task.md Dependencies — task.md is source of truth)

TASK-013
  ├── TASK-014   (blocked by TASK-013)
  │   └── TASK-018   (blocked by TASK-014, TASK-015, TASK-016, TASK-017)
  │       └── TASK-019   (blocked by TASK-017, TASK-018)
  └── TASK-015   (blocked by TASK-012, TASK-013) [waits on TASK-012 (in_progress)]
      └── TASK-018   (blocked by TASK-014, TASK-015, TASK-016, TASK-017)

TASK-016   [waits on TASK-012 (in_progress)]
  └── TASK-018   (blocked by TASK-014, TASK-015, TASK-016, TASK-017)

TASK-017
  ├── TASK-018   (blocked by TASK-014, TASK-015, TASK-016, TASK-017)
  └── TASK-019   (blocked by TASK-017, TASK-018)

Soft / unblocks:
- TASK-013 prefer after TASK-004
- TASK-013 prefer after TASK-011
- TASK-015 prefer after TASK-014

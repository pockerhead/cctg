# Roadmap graph (derived from task.md Dependencies — task.md is source of truth)

TASK-008
  ├── TASK-009   (blocked by TASK-008)
  └── TASK-011   (blocked by TASK-008, TASK-010)
      ├── TASK-012   (blocked by TASK-010, TASK-011)
      │   ├── TASK-015   (blocked by TASK-007, TASK-011, TASK-012, TASK-013) [waits on TASK-007 (in_progress)]
      │   │   └── TASK-018   (blocked by TASK-014, TASK-015, TASK-016, TASK-017)
      │   │       └── TASK-019   (blocked by TASK-017, TASK-018)
      │   └── TASK-016   (blocked by TASK-011, TASK-012)
      │       └── TASK-018   (blocked by TASK-014, TASK-015, TASK-016, TASK-017)
      ├── TASK-014   (blocked by TASK-011, TASK-013)
      │   └── TASK-018   (blocked by TASK-014, TASK-015, TASK-016, TASK-017)
      ├── TASK-015   (blocked by TASK-007, TASK-011, TASK-012, TASK-013) [waits on TASK-007 (in_progress)]
      ├── TASK-016   (blocked by TASK-011, TASK-012)
      └── TASK-017   (blocked by TASK-011)
          ├── TASK-018   (blocked by TASK-014, TASK-015, TASK-016, TASK-017)
          └── TASK-019   (blocked by TASK-017, TASK-018)

TASK-010
  ├── TASK-011   (blocked by TASK-008, TASK-010)
  ├── TASK-012   (blocked by TASK-010, TASK-011)
  └── TASK-013   (blocked by TASK-010)
      ├── TASK-014   (blocked by TASK-011, TASK-013)
      └── TASK-015   (blocked by TASK-007, TASK-011, TASK-012, TASK-013) [waits on TASK-007 (in_progress)]

Soft / unblocks:
- TASK-011 prefer after TASK-004
- TASK-012 prefer after TASK-003
- TASK-013 prefer after TASK-004
- TASK-013 prefer after TASK-011
- TASK-015 prefer after TASK-014

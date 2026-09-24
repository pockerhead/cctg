## Counter-example tested

If a dead slot can receive a new top-level session through ordinary slot reuse without any transition that the registry/slots actor can distinguish as “the session in this slot became alive,” then the stated success predicate (“deliver the buffer when the session in the slot revives”) is incomplete: an implementation could satisfy a narrow resume-path test while buffered messages remain stranded when the slot becomes alive through normal session succession.

## Orchestrator note (2026-09-24)

The codex run stopped after this section: the host refused to start processes (STATUS_DLL_INIT_FAILED) and the run could not finish its investigation. The counter-example is accepted on its face by the orchestrator without re-running: the slot actor already moves a new top-level session into the first slot of its folder without a live session (TASK-011 rule), so "the session in the slot revives" must mean any live top-level session becoming the slot's current session (resume of the same session, a new session taking the slot, /clear), not only the Resume button path. Treated as PREMISE SUSPECT with the spec amended; see OPEN_DECISIONS.md.

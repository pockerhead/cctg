# Disconfirmation test

Concrete counterexample: while the slot is in `waiting for permission`, the
status message still offers Interrupt; after confirmation the agent injects
one Esc into Claude's open permission dialog. If Esc only selects/returns the
deny outcome and Claude continues the turn, then the implementation does not
satisfy "interrupt stops the running turn" and the reviewed design is wrong.

Evidence to seek before review: callback gating/rendering for the waiting
phase, console-key dispatch, tests covering an open permission prompt, and the
probe's actual state when Esc was injected.

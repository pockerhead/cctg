# Disconfirmation case

Concrete counter-example: a transcript chunk contains `Call(A), Call(B), Result(B), Result(A)`. The task requires completed tool messages in call order. If the proposed implementation emits B when its result arrives instead of waiting for A, the reviewed design is wrong even though no event is lost.

Search target: `scratch/planner/ws/` stream reducer, pending-call representation, and tests asserting order under out-of-order results.

## Result

The counter-example holds. `crates/cctg/src/hub/stream.rs:94-106` finds the matching pending call and immediately appends its message when each result is read. Therefore `Result(B), Result(A)` emits B then A. The test named `a_call_line_goes_out_with_its_result_in_call_order` uses only `Result(A), Result(B)` and does not exercise the disconfirming order.

# Disconfirmation target

Concrete counterexample: after session A ends in folder F, sessions B and C start
concurrently in F. If allocation observes the freed slot non-atomically, both can claim
ordinal 1, or both can independently decide to create a replacement topic. Correct
behavior is that exactly one takes the existing slot/topic and the other gets ordinal 2
with exactly one new topic.

Status: disproved by inspection. `Slots` is the sole owner of `Registry`, and
`Registry::allocate` plus `occupy` run synchronously while processing one event, so
the second start observes the first claimant. The existing
`concurrent_sessions_get_new_ordinals_and_free_slots_are_reused_first` test covers
sequential actor-visible starts, but there is no exact regression test with one dead
slot followed by two queued starts; the revised plan should add it.

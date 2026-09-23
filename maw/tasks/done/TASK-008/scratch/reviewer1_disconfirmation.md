# Disconfirmation case

Concrete counterexample: enqueue ordinary message `A` for topic 7, then enqueue
permission message `B` for the same topic. If the scheduler's permission lane
sends `B` before `A`, the plan's unconditional claim that it preserves FIFO
within a topic is false.

Result: the counterexample holds. `LANES` checks `Permission` before `Message`,
`enqueue` puts the two sends in different queues, and `pick` always selects the
permission head first. The existing priority test uses different topics and
does not test the acceptance criterion's same-topic FIFO requirement.

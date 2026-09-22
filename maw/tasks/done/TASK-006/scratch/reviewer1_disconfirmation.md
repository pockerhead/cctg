Counter-example tested before review evaluation:

An assistant `Turn` containing text with `stop_reason: None` and no later tool call.
The task criterion says brief may show assistant answer text only from a record whose
`stop_reason == "end_turn"`. If the prototype renders this text in brief, the plan's
answer-classification rule is disproved.

# plan-reviewer-2 disconfirmation (written before evaluation)

Most concrete counter-example that would make PLAN_V2 wrong:
an ordinary `Send` A for topic 7 is queued, then a permission `Send` B for the
same topic 7; the scheduler dispatches B before A. The spec requires FIFO within
a topic, so any scheduler that sends B,A is wrong, and a plan that ships it is wrong.

Secondary counter-example: a malformed `.env` (unterminated quote on a line
before CCTG_BOT_TOKEN / CCTG_ALLOWED_USER_IDS) makes `cctg hub --env-file bad.env`
print those values to stderr through the dotenvy::Error Display.

Search: reproduced both in scratch/reviewer2/ws (unmodified planner reference)
with probe tests, see repro.out.txt.

Result: HELD (both). scratch/reviewer2/repro.out.txt: against the unmodified
planner reference, permission_never_overtakes_its_own_topic sent
["prompt","ordinary","doc"]; malformed_env_file_does_not_echo_its_contents found the
synthetic token marker and allowlist id in `cctg hub --env-file` stderr.
Unplanned third finding while verifying: the reference unit test
routing_logs_never_contain_user_ids is flaky (56/100 lib runs failed), see flake.out.txt.

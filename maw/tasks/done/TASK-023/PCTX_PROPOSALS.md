# PCTX proposals (TASK-023)

## 2026-09-24 — hub domain, TASK-016 bullet
Replace the sentence "Known follow-up (TASK-023): the held Stop answer is not gated by `broken`, so after a refusal it can arrive before its tool lines." with:
"TASK-023: a streamed session's answer (Stop hook) rides the stream as `Op::Stream` (`merge: false`) and is tracked in `Live` with its text; after a refusal it is dropped behind the refused line like the lines, the rewind holds it again (not before `retry + hold_answer`) and the re-read turn end lets it go after its lines. An answer that prefers a file still goes as a document outside the stream. Known gap: a session that ends while its stream waits for a rewind loses the refused lines (its `Live` is dropped once nothing is in flight)."
Why: the follow-up is closed; the rotation gap is confirmed by a probe (scratch/implementer/rotation_probe.*) and not fixed here.

## 2026-09-24 — hub domain, TASK-016/023 bullet (fixer addition)
Add: "An answer is paired with its turn end by the transcript byte of that turn end (`Held::end`), never by count: a turn end read again after a rewind lets only its own answer go, or nothing when that answer is already in the topic (`Live::turn_end`); an answer held again whose turn end is behind the committed offset goes first on the re-read. A streamed answer counts against the 256 pending-send cap like a plain one (`answer_ops` room check; over the cap it is dropped with the one overflow warn and nothing of it is tracked in the stream). Known limits: a re-held answer can go by its timeout ahead of its re-read lines; a same-topic permission prompt can overtake a queued answer; repeated refusals push the answer back once per rewind; a document answer is not gated."
Why: review I1/I2 of TASK-023 (count pairing let a later answer out before its lines; streamed answers bypassed the cap).

> RESOLVED: both folded into domains/hub.md (TASK-016 bullet, with the QA round 2 answered-mark rule) on 2026-09-24.

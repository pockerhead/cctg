# Disconfirmation case (recorded before evaluation)

Two processes for the same session (a hook and its agent) concurrently replay the same spool file. One POST succeeds and removes the file while the other POST fails. The implementation is wrong if the racing delete or the failed duplicate can lose the event, permanently block later delivery, or make the current hook overtake the retained event.

Status: the narrow case did not lose the saved event. `spool::replay` removes
only after a successful POST and ignores a racing `NotFound`; `serve_hooks`
checks and inserts `event_id` under one mutex, so concurrent accepted copies
produce one actor event. The reference has no direct concurrent-replay test,
so one must be added. The broader concurrency claim does fail: `save()` checks
the per-session and global counts before writing with no inter-process lock,
therefore two hook processes can both observe room and exceed either bound.

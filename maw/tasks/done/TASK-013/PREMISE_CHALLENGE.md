# Premise challenge: TASK-013

## 1. Counter-example tested

The spawned `cctg agent` process does not actually receive a usable `CLAUDE_CODE_SESSION_ID`; if true, the proposed persistent hub registration cannot be matched to the hook session, so the stated protocol surface could pass while the real routing requirement remains broken.

## 2. Primary-source investigation

The probe records the process environment directly with `os.environ.get` at `maw/tasks/done/TASK-004/scratch/probe_channel_server.py:88-97`. I parsed only the first raw `start` record from three probe logs. The command returned non-empty `CLAUDE_CODE_SESSION_ID` values for interactive `I4`, headless-resume `M2_headless_resume`, and `N5` at `maw/tasks/done/TASK-004/scratch/probe_log_I4.jsonl:1`, `probe_log_M2.jsonl:1`, and `probe_log_N5.jsonl:1`.

I then compared the M2 probe value with the raw command stream rather than a summary. The executed `Select-String` extraction returned:

```text
{"Path":"C:\\Users\\user\\dev\\cctg\\maw\\tasks\\done\\TASK-004\\scratch\\run_M2_stream.jsonl","LineNumber":2,"SessionId":"684d8e75-12f4-49a0-83cd-f706b8686468"}
```

That exactly matches `CLAUDE_CODE_SESSION_ID` in `maw/tasks/done/TASK-004/scratch/probe_log_M2.jsonl:1`. An attempted comparison with `interactive_I4_session.txt` returned an empty `session_file` and therefore supplied no evidence either way.

Finally, the existing hub consumes the registered string as the session key and either binds it immediately or holds it pending the matching hook session at `crates/cctg/src/hub/slots.rs:252-274`; when that hook session appears, it binds the pending connection by the same key at `crates/cctg/src/hub/slots.rs:299-305`. The wire contract names this exact field at `crates/cctg/src/wire.rs:112-118`.

## 3. Did it hold

No. The tested counter-example did not hold: raw process evidence contains a non-empty session id, the M2 raw execution identifies the same session id, and the existing hub matches agent registration to hook state by that exact string. The empty redacted interactive comparison file is absence of confirmation, not positive contrary evidence.

## 4. Verdict

PREMISE HOLDS — `maw/tasks/done/TASK-004/scratch/probe_log_M2.jsonl:1` and `run_M2_stream.jsonl:2` contain the same session id captured directly by `probe_channel_server.py:88-97`, and `crates/cctg/src/hub/slots.rs:252-274,299-305` uses that id to bind the agent to the hook-announced session.
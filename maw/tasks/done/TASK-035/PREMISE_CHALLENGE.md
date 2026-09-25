# PREMISE_CHALLENGE — TASK-035

## 1. Counter-example tested

The premise opens with "После TASK-034 hub не зависит от файлов машин" and then scopes the remote hub as "listen address + TLS + Linux image + CI + role split". Counter-example: the hub code path still depends on a file that is shared with the client machine (a `transcript_path`, `~/.claude/projects`, a `subagents/agent-*.jsonl`, the client's executable, or a Windows-only API compiled into the hub path). If it does, a hub in a Linux container is broken on arrival, and the listed scope (network + packaging) can pass every acceptance criterion while a hub/client feature stays broken.

## 2. Primary-source investigation

1. `grep -rn "std::fs|tokio::fs|File::open|transcript_path|projects" crates/cctg/src/hub/*.rs`: outside `#[cfg(test)]` code the hub only touches its own state files: `hub/offset.rs:29-72` (offset), `hub/registry.rs:1696-1755` (registry.json). `transcript_path` in `hub/registry.rs:339,852,1055` and `hub/slots.rs:1457,2044,3011-3016` is stored as a string and handed to an agent that `reads` (`bound.reads && bound.session == session`), so the hub does not open it. On this point the TASK-034 claim holds.
2. Windows-only code: `grep -rn "cfg(windows)|windows_sys"`. All hits are gated: `keys.rs`, `proctree.rs` (it has a `cfg(target_os = "linux")` branch at 192/240/404), `agent.rs`, `run.rs`, `supervise.rs`, `tail.rs`, `reads.rs`. `windows-sys` is a `[target.'cfg(windows)'.dependencies]` in `crates/cctg/Cargo.toml`. I found no ungated Windows API in the hub path. I did not run a Linux build (no Linux toolchain was used here), so this part is not verified by execution.
3. The client's executable, which is the part that did break:
   - `crates/cctg/src/client.rs:1-6`: "The build is the sha256 of the executable file ... two processes run the same build exactly when their files had the same bytes ... the hub compares builds, not versions".
   - `crates/cctg/src/hub/mod.rs:218-239`: the hub computes `build = crate::client::own_build()` (sha256 of the hub's own `current_exe`) and passes it into `slots::Options { build, .. }`.
   - `crates/cctg/src/wire.rs:170-180`: `Client { version, build: sha256 of the agent's executable, self_update }` is the only build identity an agent sends.
   - `crates/cctg/src/hub/slots.rs:3992-4001` `outdated()`: an agent is outdated when `client.build != hub`, meaning its sha256 differs from the hub executable's sha256.
   - `crates/cctg/src/hub/slots.rs:4006-4045` `warn_outdated()` (called from `slots.rs:5271`) sends a loud (`notify: true`) message `status::outdated_text` ("Клиент cctg в этой сессии устарел ... нажмите «Обновить»", `hub/status.rs:108-112`) with the update keyboard to every live session whose agent is `outdated`, once per hub build per session. `slots.rs:4258`: after a press the agent answers `UpToDate` and the user gets `NO_NEW_BUILD_NOTICE`.
   - `crates/cctg/src/device.rs:13-14,27-29`: `CCTG_HUB_HOOK_ADDR` and `CCTG_HUB_AGENT_ADDR` already come from `device.env`, so that part of the premise is correct.

## 3. Did it hold

Partly. The hub no longer reads transcript or session files (TASK-034 did what it says), and the Windows code is cfg-gated. But the hub still assumes that it and every client run from byte-identical executables. The "is this agent up to date" check compares the sha256 of the hub's own file with the sha256 of the client's file (`slots.rs:3999`). A hub built for Linux in a Docker image (and, even more so, the separate server binary or cargo feature the premise asks for in "Разделение ролей в сборке") can never have the same bytes as a Windows client built from the same commit. After the task as written, every agent of every live session is always `outdated`: each session gets a loud false "устарел, нажмите Обновить" message, and pressing the button leads to "no new build" (`slots.rs:4258`). None of the acceptance criteria (TLS e2e, localhost unchanged, Docker/compose/healthcheck, CI, secrets, existing tests) would catch this. Existing tests use a fixed `HUB_BUILD` constant (`slots.rs:12198`), so they stay green.

The premise does not mention build identity or the update or outdated flow at all. Its sentence "hub не зависит от файлов машин" is incomplete: the hub still depends on being the same file as the client.

## 4. Verdict

PREMISE SUSPECT — `crates/cctg/src/hub/mod.rs:218` (hub build = sha256 of its own executable) + `crates/cctg/src/hub/slots.rs:3992-4001` (`outdated` = `client.build != hub`) + `slots.rs:4006-4045` (loud "устарел / Обновить" per live session): the hub still assumes it runs from the same executable file as its clients. A Linux or role-split server binary breaks that for every Windows client, and every stated acceptance criterion can still pass ; smallest implied reframing: "remote-ready" must also cover how the hub tells whether a client is current when the hub and the clients no longer run from one shared executable (another OS, another binary), and not only the transport, packaging and CI.

//! TASK-039: a session whose claude process died without a SessionEnd gives
//! its topic to the next session of the folder. The list of live claude pids
//! comes from a real `cctg hook SessionStart`; the dead session's pid is a
//! real process named `claude(.exe)` that was killed, the live one is still
//! running.

#![cfg(any(windows, target_os = "linux"))]

use std::io::Write;
use std::net::{Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use cctg::hub::ingress;
use cctg::hub::registry::{Registry, RegistryStore, SlotId};
use cctg::wire::{HookEvent, HookPost, Secret};
use tokio::sync::mpsc;

const SECRET: &str = "reap-e2e-secret-0123456789";
const STAND_IN: &str = "CCTG_REAP_STAND_IN";
const DEAD: &str = "aaaaaaaa-0000-4000-8000-000000000001";
const LIVE: &str = "bbbbbbbb-0000-4000-8000-000000000002";
const FAR: &str = "cccccccc-0000-4000-8000-000000000003";
const NO_PID: &str = "dddddddd-0000-4000-8000-000000000004";
const NEXT: &str = "eeeeeeee-0000-4000-8000-000000000005";

/// The Claude Code stand-in: a copy of this binary named `claude(.exe)` runs
/// this test, which blocks until its stdin closes. A plain run returns.
#[test]
#[ignore = "helper process of reap_e2e"]
fn stand_in() {
    if std::env::var_os(STAND_IN).is_some() {
        let mut sink = Vec::new();
        let _ = std::io::Read::read_to_end(&mut std::io::stdin(), &mut sink);
    }
}

fn root() -> PathBuf {
    let root = Path::new(env!("CARGO_TARGET_TMPDIR")).join("reap-e2e");
    std::fs::create_dir_all(root.join(".cctg")).unwrap();
    root
}

fn stand_in_binary(root: &Path) -> PathBuf {
    let bin = root.join(if cfg!(windows) {
        "claude.exe"
    } else {
        "claude"
    });
    if !bin.exists() {
        std::fs::copy(std::env::current_exe().unwrap(), &bin).expect("copy the stand-in");
    }
    bin
}

fn spawn_claude(bin: &Path) -> Child {
    Command::new(bin)
        .args(["--exact", "stand_in", "--ignored", "--nocapture"])
        .env(STAND_IN, "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("start the stand-in")
}

fn post(host: &str, session: &str, cwd: &str, pid: Option<u32>) -> HookPost {
    HookPost::new(
        host.into(),
        session.into(),
        cwd.into(),
        String::new(),
        HookEvent::SessionStart {
            source: Some("startup".into()),
            claude_pid: pid,
            parent_claude_pid: None,
        },
    )
}

#[tokio::test(flavor = "multi_thread")]
async fn a_start_after_a_killed_session_takes_its_topic() {
    let root = root();
    let bin = stand_in_binary(&root);
    // LIVE's process, the new session's own process, and a killed one.
    let mut live = spawn_claude(&bin);
    let mut own = spawn_claude(&bin);
    let mut dead = spawn_claude(&bin);
    let (live_pid, own_pid, dead_pid) = (live.id(), own.id(), dead.id());
    // Never created: the hook keeps an unresolvable cwd as it is, so the
    // registry and the hook spell the folder the same way.
    let folder = root.join("project-folder").to_string_lossy().into_owned();
    assert!(!Path::new(&folder).exists());
    dead.kill().unwrap();
    dead.wait().unwrap();

    // The hub as it was before: the killed session holds the folder's topic.
    let mut registry = Registry::default();
    registry.apply_hook(&post("box", DEAD, &folder, Some(dead_pid)));
    registry.apply_hook(&post("box", LIVE, "/work/other", Some(live_pid)));
    registry.apply_hook(&post("far", FAR, &folder, Some(dead_pid)));
    registry.apply_hook(&post("box", NO_PID, "/work/third", None));
    registry.topic_created(SlotId(0), 100, "project", None);
    // Saved and loaded: the starts are older than any grace.
    let store = RegistryStore::open(&root).unwrap();
    store.save(&RegistryStore::encode(&registry)).unwrap();
    let mut registry = store.load().unwrap();

    // The next session in the folder, through the real hook.
    let listener = ingress::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))
        .await
        .unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let (tx, mut events) = mpsc::channel(4);
    tokio::spawn(ingress::serve_hooks(
        listener,
        Secret::parse(SECRET).unwrap(),
        tx,
    ));
    std::fs::write(
        root.join(".cctg").join("device.env"),
        format!("CCTG_HUB_SECRET={SECRET}\nCCTG_HUB_HOOK_ADDR={addr}\nCCTG_HOST=box\n"),
    )
    .unwrap();
    let input = serde_json::json!({
        "session_id": NEXT,
        "cwd": folder,
        "transcript_path": "",
        "hook_event_name": "SessionStart",
        "source": "startup",
    })
    .to_string();
    let home = root.clone();
    let output = tokio::task::spawn_blocking(move || {
        let mut child = Command::new(env!("CARGO_BIN_EXE_cctg"))
            .args(["hook", "SessionStart"])
            .env("USERPROFILE", &home)
            .env("HOME", &home)
            .env("CLAUDE_PID", own_pid.to_string())
            .env_remove("CLAUDE_CODE_SESSION_ID")
            .env_remove("CCTG_HUB_SECRET")
            .env_remove("CCTG_HUB_HOOK_ADDR")
            .env_remove("CCTG_HOST")
            .env_remove("CCTG_STATE_DIR")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("cctg starts");
        let _ = child.stdin.take().unwrap().write_all(input.as_bytes());
        child.wait_with_output().unwrap()
    })
    .await
    .unwrap();
    assert!(output.status.success());
    let mut next = tokio::time::timeout(Duration::from_secs(5), events.recv())
        .await
        .expect("hub got the start")
        .unwrap();
    let pids = next
        .live_claude_pids
        .clone()
        .expect("the start lists live pids");
    assert!(pids.contains(&live_pid), "{pids:?}");
    assert!(pids.contains(&own_pid), "{pids:?}");
    assert!(!pids.contains(&dead_pid), "{pids:?}");
    // The lineage depends on where the test runs (under claude or not; see
    // proctree tests); the session is its own stand-in's, top-level.
    next.event = HookEvent::SessionStart {
        source: Some("startup".into()),
        claude_pid: Some(own_pid),
        parent_claude_pid: None,
    };
    assert_eq!(next.cwd, folder);

    let followup = registry.apply_hook(&next);
    for child in [&mut live, &mut own] {
        let _ = child.kill();
        let _ = child.wait();
    }

    assert_eq!(followup.reaped, [DEAD]);
    assert!(registry.sessions[DEAD].ended);
    for kept in [LIVE, FAR, NO_PID, NEXT] {
        assert!(!registry.sessions[kept].ended, "{kept}");
    }
    let slot = registry.sessions[NEXT].slot.expect("a slot");
    assert_eq!(slot, SlotId(0), "the killed session's topic");
    assert_eq!(registry.slots[slot.0].ordinal, 1);
    assert_eq!(registry.slots[slot.0].topic_id, Some(100));
    assert!(registry.slots.iter().all(|slot| slot.ordinal == 1), "no #2");
}

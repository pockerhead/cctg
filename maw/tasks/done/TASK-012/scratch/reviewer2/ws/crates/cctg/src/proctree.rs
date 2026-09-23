//! Which claude process runs this hook, and which claude process (if any)
//! runs that one: the nesting evidence of TASK-003 (`FINDINGS.md`, Контракт).
//!
//! Env cannot tell a nested `claude -p` from a top-level session: every claude
//! overwrites `CLAUDE_CODE_SESSION_ID` and `CLAUDE_PID` with its own values.
//! The process tree can, as long as the processes between the two claudes are
//! still alive when the hook runs (a blocking Bash tool call keeps them).
//!
//! Windows reads one ToolHelp snapshot (about 7 ms for ~450 processes on the
//! dev host); Linux follows `/proc/<pid>/stat` upward; other systems report no
//! chain. Nothing here spawns a process.

/// One process of the ancestor chain; `chain[0]` is the hook itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Proc {
    pub pid: u32,
    pub name: String,
}

/// What the hook reports to the hub.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Lineage {
    /// The claude process of this session.
    pub claude_pid: Option<u32>,
    /// The next claude process above it: this session is nested. `None` means
    /// top-level as far as this device can see.
    pub parent_claude_pid: Option<u32>,
}

/// Longest chain followed; real chains are under 20 processes.
const MAX_DEPTH: usize = 64;

/// Walks the tree of the current process. `env_*` are `CLAUDE_PID` and
/// `CLAUDE_CODE_SESSION_ID` as the hook sees them.
pub fn current_lineage(
    env_claude_pid: Option<u32>,
    env_session_id: Option<&str>,
    stdin_session_id: &str,
) -> Lineage {
    let chain = ancestors(std::process::id()).unwrap_or_default();
    lineage(&chain, env_claude_pid, env_session_id, stdin_session_id)
}

/// Pure decision over a chain (`chain[0]` = the hook).
///
/// - The session's own claude is the nearest claude ancestor: the hook is
///   spawned by it (directly or through a shell), never by another claude.
///   Exception: a `node` ancestor whose pid equals `CLAUDE_PID` and which is
///   nearer than that claude (an npm install nested in a native session).
///   When the chain has no claude by name (unknown platform, a renamed
///   binary), the ancestor whose pid equals `CLAUDE_PID` is used, and without
///   one `CLAUDE_PID` itself.
/// - The parent is the next claude ancestor above the own one, by name only:
///   a `node` parent (npm-only install) is not recognised.
/// - `CLAUDE_CODE_SESSION_ID != stdin session_id` (never observed, TASK-003
///   rule 1) means the env was inherited from a parent session: then
///   `CLAUDE_PID` is the parent's pid, used only when the walk found no parent.
pub fn lineage(
    chain: &[Proc],
    env_claude_pid: Option<u32>,
    env_session_id: Option<&str>,
    stdin_session_id: &str,
) -> Lineage {
    let ancestors = chain.get(1..).unwrap_or_default();
    let named = ancestors.iter().position(|proc| is_claude(&proc.name));
    let by_env = env_claude_pid.and_then(|pid| ancestors.iter().position(|proc| proc.pid == pid));
    let own_index = match (named, by_env) {
        // An npm-installed claude (`node`) below a native one: the env pid
        // names this session, the native claude above it is the parent.
        (Some(named), Some(env)) if env < named && has_stem(&ancestors[env].name, "node") => {
            Some(env)
        }
        (Some(named), _) => Some(named),
        (None, env) => env,
    };
    let claude_pid = own_index
        .map(|index| ancestors[index].pid)
        .or(env_claude_pid);
    let mut parent_claude_pid = own_index.and_then(|index| {
        ancestors[index + 1..]
            .iter()
            .find(|proc| is_claude(&proc.name) && Some(proc.pid) != claude_pid)
            .map(|proc| proc.pid)
    });
    let env_is_foreign = env_session_id
        .is_some_and(|id| !id.is_empty() && !stdin_session_id.is_empty() && id != stdin_session_id);
    if parent_claude_pid.is_none() && env_is_foreign && env_claude_pid != claude_pid {
        parent_claude_pid = env_claude_pid;
    }
    Lineage {
        claude_pid,
        parent_claude_pid,
    }
}

/// `claude.exe` on Windows, `claude` elsewhere; case-insensitive.
fn is_claude(name: &str) -> bool {
    has_stem(name, "claude")
}

/// `name` is `stem` or `stem.exe`, ignoring ASCII case.
fn has_stem(name: &str, stem: &str) -> bool {
    let base = name
        .len()
        .checked_sub(4)
        .filter(|&cut| name.is_char_boundary(cut) && name[cut..].eq_ignore_ascii_case(".exe"))
        .map_or(name, |cut| &name[..cut]);
    base.eq_ignore_ascii_case(stem)
}

/// The chain from `pid` upward, `pid` first. `None` when this platform has no
/// supported source; a chain that stops early is returned as far as it goes.
pub fn ancestors(pid: u32) -> Option<Vec<Proc>> {
    let table = table()?;
    let mut chain = Vec::new();
    let mut current = pid;
    while chain.len() < MAX_DEPTH {
        let Some((parent, name)) = table.lookup(current) else {
            break;
        };
        if chain.iter().any(|proc: &Proc| proc.pid == current) {
            break;
        }
        chain.push(Proc { pid: current, name });
        if parent == 0 || parent == current {
            break;
        }
        current = parent;
    }
    Some(chain)
}

#[cfg(windows)]
fn table() -> Option<windows::Snapshot> {
    windows::Snapshot::take()
}

#[cfg(target_os = "linux")]
fn table() -> Option<linux::Proc> {
    Some(linux::Proc)
}

#[cfg(not(any(windows, target_os = "linux")))]
fn table() -> Option<NoTable> {
    None
}

#[cfg(not(any(windows, target_os = "linux")))]
struct NoTable;

#[cfg(not(any(windows, target_os = "linux")))]
impl NoTable {
    fn lookup(&self, _pid: u32) -> Option<(u32, String)> {
        None
    }
}

#[cfg(windows)]
mod windows {
    use std::collections::HashMap;

    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW,
        TH32CS_SNAPPROCESS,
    };

    /// pid -> (parent pid, image name) at one instant.
    pub struct Snapshot(HashMap<u32, (u32, String)>);

    impl Snapshot {
        pub fn take() -> Option<Self> {
            let mut table = HashMap::new();
            // SAFETY: the snapshot handle is checked, used only in this block
            // and closed exactly once; `entry` is a plain C struct whose
            // `dwSize` is set as the API requires before the first call.
            unsafe {
                let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
                if snapshot == INVALID_HANDLE_VALUE {
                    return None;
                }
                let mut entry: PROCESSENTRY32W = std::mem::zeroed();
                entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
                let mut ok = Process32FirstW(snapshot, &mut entry);
                while ok != 0 {
                    let name = &entry.szExeFile;
                    let len = name.iter().position(|&c| c == 0).unwrap_or(name.len());
                    table.insert(
                        entry.th32ProcessID,
                        (
                            entry.th32ParentProcessID,
                            String::from_utf16_lossy(&name[..len]),
                        ),
                    );
                    ok = Process32NextW(snapshot, &mut entry);
                }
                CloseHandle(snapshot);
            }
            Some(Self(table))
        }

        pub fn lookup(&self, pid: u32) -> Option<(u32, String)> {
            self.0.get(&pid).cloned()
        }
    }
}

#[cfg(target_os = "linux")]
mod linux {
    /// Reads `/proc/<pid>/stat` on demand.
    pub struct Proc;

    impl Proc {
        pub fn lookup(&self, pid: u32) -> Option<(u32, String)> {
            let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
            super::parse_stat(&stat)
        }
    }
}

/// `pid (comm) state ppid ...`; `comm` may itself contain `)` and spaces.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn parse_stat(stat: &str) -> Option<(u32, String)> {
    let open = stat.find('(')?;
    let close = stat.rfind(')')?;
    let name = stat.get(open + 1..close)?.to_owned();
    let mut rest = stat.get(close + 1..)?.split_whitespace();
    let _state = rest.next()?;
    let parent = rest.next()?.parse().ok()?;
    Some((parent, name))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chain(nodes: &[(u32, &str)]) -> Vec<Proc> {
        nodes
            .iter()
            .map(|&(pid, name)| Proc {
                pid,
                name: name.to_owned(),
            })
            .collect()
    }

    const SID: &str = "4abcca41-a869-41e8-a7a3-3760d0ebfbb4";

    // Chains copied from maw/tasks/done/TASK-003/scratch/capture_*.jsonl
    // (hook = python.exe there, cctg.exe in production).

    #[test]
    fn top_level_session_has_no_parent() {
        // capture_B_nested.jsonl #0 (outer session, chain ends at env.exe).
        let nodes = chain(&[
            (35960, "python.exe"),
            (32724, "bash.exe"),
            (29164, "bash.exe"),
            (776, "claude.exe"),
            (34164, "env.exe"),
        ]);
        assert_eq!(
            lineage(&nodes, Some(776), Some(SID), SID),
            Lineage {
                claude_pid: Some(776),
                parent_claude_pid: None
            }
        );
    }

    #[test]
    fn nested_session_reports_the_next_claude() {
        // capture_B_nested.jsonl #1 and #3 (SessionStart and SessionEnd).
        let nodes = chain(&[
            (14588, "python.exe"),
            (17412, "bash.exe"),
            (27048, "bash.exe"),
            (25388, "claude.exe"),
            (20976, "bash.exe"),
            (31964, "bash.exe"),
            (2168, "bash.exe"),
            (776, "claude.exe"),
            (34164, "env.exe"),
        ]);
        let want = Lineage {
            claude_pid: Some(25388),
            parent_claude_pid: Some(776),
        };
        assert_eq!(lineage(&nodes, Some(25388), Some(SID), SID), want);
        // Stale CLAUDE_PID that is nowhere in the chain: position decides.
        assert_eq!(lineage(&nodes, Some(4242), Some(SID), SID), want);
        // No CLAUDE_PID at all.
        assert_eq!(lineage(&nodes, None, None, SID), want);
    }

    #[test]
    fn env_stripped_nested_run_and_interactive_start() {
        // capture_D_nested_env_stripped.jsonl #1.
        let nodes = chain(&[
            (32380, "python.exe"),
            (10064, "bash.exe"),
            (28028, "bash.exe"),
            (16024, "claude.exe"),
            (29360, "bash.exe"),
            (29560, "bash.exe"),
            (28544, "bash.exe"),
            (7100, "claude.exe"),
            (32968, "env.exe"),
        ]);
        assert_eq!(
            lineage(&nodes, Some(16024), Some(SID), SID).parent_claude_pid,
            Some(7100)
        );
        // capture_E_interactive.jsonl #0: parent claude is alive two levels up.
        let nodes = chain(&[
            (33220, "python.exe"),
            (35944, "bash.exe"),
            (15248, "bash.exe"),
            (28764, "claude.exe"),
            (31852, "python.exe"),
            (13672, "bash.exe"),
            (35000, "bash.exe"),
            (30500, "bash.exe"),
            (15320, "claude.exe"),
            (34616, "bash.exe"),
            (15580, "bash.exe"),
        ]);
        assert_eq!(
            lineage(&nodes, Some(28764), Some(SID), SID),
            Lineage {
                claude_pid: Some(28764),
                parent_claude_pid: Some(15320)
            }
        );
    }

    #[test]
    fn exec_form_hook_is_a_direct_child_of_claude() {
        let nodes = chain(&[(1, "cctg.exe"), (2, "Claude.EXE"), (3, "explorer.exe")]);
        assert_eq!(
            lineage(&nodes, Some(2), Some(SID), SID),
            Lineage {
                claude_pid: Some(2),
                parent_claude_pid: None
            }
        );
    }

    #[test]
    fn no_chain_falls_back_to_env_pid_and_top_level() {
        assert_eq!(
            lineage(&[], Some(10), Some(SID), SID),
            Lineage {
                claude_pid: Some(10),
                parent_claude_pid: None
            }
        );
        assert_eq!(lineage(&[], None, None, SID), Lineage::default());
    }

    #[test]
    fn a_node_claude_below_a_native_claude_is_the_own_process() {
        // npm-installed `claude -p` run from a native session's Bash tool:
        // CLAUDE_PID names the node process, the native claude is the parent.
        let nodes = chain(&[
            (1, "cctg.exe"),
            (2, "bash.exe"),
            (3, "node.exe"),
            (4, "bash.exe"),
            (5, "claude.exe"),
        ]);
        assert_eq!(
            lineage(&nodes, Some(3), Some(SID), SID),
            Lineage {
                claude_pid: Some(3),
                parent_claude_pid: Some(5)
            }
        );
    }

    #[test]
    fn npm_only_chain_keeps_the_own_pid_and_finds_no_parent() {
        // Unsupported deployment (no Node on any machine): a node parent is
        // not recognised by image name, so the run looks top-level.
        let nodes = chain(&[
            (1, "cctg.exe"),
            (2, "bash.exe"),
            (3, "node.exe"),
            (4, "bash.exe"),
            (5, "node.exe"),
        ]);
        assert_eq!(
            lineage(&nodes, Some(3), Some(SID), SID),
            Lineage {
                claude_pid: Some(3),
                parent_claude_pid: None
            }
        );
    }

    #[test]
    fn a_stale_env_pid_on_a_wrapper_does_not_move_the_own_process() {
        // CLAUDE_PID reused by a shell between the hook and its claude.
        let nodes = chain(&[
            (1, "cctg.exe"),
            (2, "bash.exe"),
            (3, "claude.exe"),
            (4, "bash.exe"),
            (5, "claude.exe"),
        ]);
        let want = Lineage {
            claude_pid: Some(3),
            parent_claude_pid: Some(5),
        };
        assert_eq!(lineage(&nodes, Some(2), Some(SID), SID), want);
        // CLAUDE_PID equal to the farther claude (a stale parent pid).
        assert_eq!(lineage(&nodes, Some(5), Some(SID), SID), want);
    }

    #[test]
    fn foreign_env_session_names_the_parent_only_as_a_fallback() {
        let nodes = chain(&[(1, "cctg.exe"), (2, "claude.exe")]);
        assert_eq!(
            lineage(&nodes, Some(9), Some("parent-session"), SID),
            Lineage {
                claude_pid: Some(2),
                parent_claude_pid: Some(9)
            }
        );
        // Same pid as our own claude: never our own parent.
        assert_eq!(
            lineage(&nodes, Some(2), Some("parent-session"), SID).parent_claude_pid,
            None
        );
    }

    #[test]
    fn claude_names() {
        for name in ["claude.exe", "CLAUDE.EXE", "claude", "Claude"] {
            assert!(is_claude(name), "{name}");
        }
        for name in [
            "claude-code.exe",
            "node.exe",
            "claude.ex",
            "xclaude",
            "",
            "é.exe",
        ] {
            assert!(!is_claude(name), "{name}");
        }
        for name in ["node.exe", "NODE.EXE", "node"] {
            assert!(has_stem(name, "node"), "{name}");
        }
        assert!(!has_stem("nodejs.exe", "node"));
    }

    #[test]
    fn proc_stat_lines() {
        assert_eq!(
            parse_stat("4242 (claude) S 4000 4242 4242 0 -1"),
            Some((4000, "claude".to_owned()))
        );
        assert_eq!(
            parse_stat("7 (a) b) c) R 1 7 7 0"),
            Some((1, "a) b) c".to_owned()))
        );
        assert_eq!(parse_stat("garbage"), None);
    }

    #[cfg(any(windows, target_os = "linux"))]
    #[test]
    fn the_live_chain_starts_at_this_process() {
        let chain = ancestors(std::process::id()).expect("supported platform");
        assert_eq!(chain.first().map(|proc| proc.pid), Some(std::process::id()));
        assert!(chain.len() >= 2, "{chain:?}");
    }
}

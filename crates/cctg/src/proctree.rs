//! Which claude process runs this hook, and which claude process (if any)
//! runs that one: the nesting evidence of TASK-003 (`FINDINGS.md`, Контракт).
//!
//! Env cannot tell a nested `claude -p` from a top-level session: every claude
//! overwrites `CLAUDE_CODE_SESSION_ID` and `CLAUDE_PID` with its own values.
//! The process tree can, as long as the processes between the two claudes are
//! still alive when the hook runs (a blocking Bash tool call keeps them).
//!
//! Windows reads one ToolHelp snapshot (about 7 ms for ~450 processes on the
//! dev host), then queries only the processes in the current ancestry for
//! their image path and creation time. Linux follows `/proc/<pid>/stat`
//! upward; other systems report no chain. Nothing here spawns a process.

use std::collections::HashMap;

/// One process of the ancestor chain; `chain[0]` is the hook itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Proc {
    pub pid: u32,
    pub name: String,
    /// Full executable path when the platform can query it. Windows always
    /// supplies this for nodes admitted to a live chain.
    pub image_path: Option<String>,
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

#[derive(Debug, Clone)]
struct ProcessEntry {
    parent: u32,
    name: String,
    image_path: Option<String>,
    created: Option<u64>,
}

/// Injectable process data. Windows fills this from one ToolHelp snapshot and
/// narrow per-ancestor queries; tests build it directly without live processes.
#[derive(Debug, Default)]
struct ProcessTable {
    entries: HashMap<u32, ProcessEntry>,
}

impl ProcessTable {
    fn insert(&mut self, pid: u32, entry: ProcessEntry) {
        self.entries.insert(pid, entry);
    }

    /// A parent must predate its child. Missing metadata or a reused pid ends
    /// the chain at the last verified process instead of following a false
    /// link into an unrelated tree.
    fn chain(&self, pid: u32) -> Vec<Proc> {
        let mut chain = Vec::new();
        let mut current = pid;
        while chain.len() < MAX_DEPTH {
            let Some(entry) = self.entries.get(&current) else {
                break;
            };
            if chain.iter().any(|proc: &Proc| proc.pid == current) {
                break;
            }
            chain.push(Proc {
                pid: current,
                name: entry.name.clone(),
                image_path: entry.image_path.clone(),
            });
            if entry.parent == 0 || entry.parent == current {
                break;
            }
            let Some(parent) = self.entries.get(&entry.parent) else {
                break;
            };
            match (parent.created, entry.created) {
                (Some(parent), Some(child)) if parent < child => {}
                _ => break,
            }
            current = entry.parent;
        }
        chain
    }
}

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
/// - The session's own claude is the nearest Claude Code CLI ancestor: the
///   hook is spawned by it (directly or through a shell), never by another
///   claude.
///   Exception: a `node` ancestor whose pid equals `CLAUDE_PID` and which is
///   nearer than that claude (an npm install nested in a native session).
///   When the chain has no claude by name (unknown platform, a renamed
///   binary), the ancestor whose pid equals `CLAUDE_PID` is used, and without
///   one `CLAUDE_PID` itself.
/// - The parent is the next verified Claude Code CLI ancestor above the own
///   one. A `node` parent (npm-only install) is not recognised.
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
    let named = ancestors.iter().position(is_cli_claude);
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
            .find(|proc| is_cli_claude(proc) && Some(proc.pid) != claude_pid)
            .map(|proc| proc.pid)
    });
    let env_is_foreign = env_session_id
        .is_some_and(|id| !id.is_empty() && !stdin_session_id.is_empty() && id != stdin_session_id);
    if parent_claude_pid.is_none() && env_is_foreign && env_claude_pid != claude_pid {
        parent_claude_pid = env_claude_pid.filter(|pid| {
            ancestors
                .iter()
                .find(|proc| proc.pid == *pid)
                .is_some_and(is_cli_claude)
        });
    }
    Lineage {
        claude_pid,
        parent_claude_pid,
    }
}

/// Longest list of live pids a hook reports; more means no list at all.
pub const MAX_LIVE_PIDS: usize = 1024;

/// Pids of every process on this device that can be a session's own claude
/// process: `claude(.exe)` by name (Claude Desktop's `claude.exe` included:
/// an extra pid only keeps a session alive) and `node(.exe)` (an npm install
/// nested in a native session, see [`lineage`]). The hub ends the sessions of
/// this host whose `claude_pid` is not listed. `None`: this platform has no
/// source, the snapshot failed, or the list is longer than
/// [`MAX_LIVE_PIDS`]; then nothing is ended.
pub fn live_claude_pids() -> Option<Vec<u32>> {
    claude_pids(platform_processes()?)
}

fn claude_pids(processes: impl IntoIterator<Item = (u32, String)>) -> Option<Vec<u32>> {
    let mut pids: Vec<u32> = processes
        .into_iter()
        .filter(|(_, name)| is_claude(name) || has_stem(name, "node"))
        .map(|(pid, _)| pid)
        .collect();
    pids.sort_unstable();
    pids.dedup();
    (pids.len() <= MAX_LIVE_PIDS).then_some(pids)
}

#[cfg(windows)]
fn platform_processes() -> Option<Vec<(u32, String)>> {
    windows::Snapshot::take().map(windows::Snapshot::processes)
}

#[cfg(target_os = "linux")]
fn platform_processes() -> Option<Vec<(u32, String)>> {
    linux::processes()
}

#[cfg(not(any(windows, target_os = "linux")))]
fn platform_processes() -> Option<Vec<(u32, String)>> {
    None
}

/// `claude.exe` on Windows, `claude` elsewhere; case-insensitive.
fn is_claude(name: &str) -> bool {
    has_stem(name, "claude")
}

fn is_cli_claude(proc: &Proc) -> bool {
    is_claude(&proc.name)
        && !proc
            .image_path
            .as_deref()
            .is_some_and(is_claude_desktop_path)
}

/// Precise Windows denylist for Claude Desktop images. The Store package and
/// the classic `AnthropicClaude` install are Electron apps, not CLI parents.
/// The Desktop-bundled CLI lives under `\Claude\claude-code\<version>\` and
/// deliberately matches neither marker.
fn is_claude_desktop_path(path: &str) -> bool {
    let normalized = path.replace('/', "\\").to_ascii_lowercase();
    normalized.contains(r"\windowsapps\claude_") || normalized.contains(r"\anthropicclaude\")
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
    platform_ancestors(pid)
}

#[cfg(target_os = "linux")]
fn platform_ancestors(pid: u32) -> Option<Vec<Proc>> {
    Some(linux::ancestors(pid))
}

#[cfg(not(any(windows, target_os = "linux")))]
fn platform_ancestors(_pid: u32) -> Option<Vec<Proc>> {
    None
}

#[cfg(windows)]
fn platform_ancestors(pid: u32) -> Option<Vec<Proc>> {
    windows::process_table(pid).map(|table| table.chain(pid))
}

#[cfg(windows)]
mod windows {
    use std::collections::HashMap;

    use windows_sys::Win32::Foundation::{CloseHandle, FILETIME, HANDLE, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW,
        TH32CS_SNAPPROCESS,
    };
    use windows_sys::Win32::System::Threading::{
        GetProcessTimes, OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
        QueryFullProcessImageNameW,
    };

    use super::{MAX_DEPTH, ProcessEntry, ProcessTable};

    const MAX_IMAGE_PATH: usize = 32_768;

    struct OwnedHandle(HANDLE);

    impl Drop for OwnedHandle {
        fn drop(&mut self) {
            // SAFETY: `OwnedHandle` is created only from a successful Win32
            // handle-returning call and owns that handle exactly once.
            unsafe {
                let _ = CloseHandle(self.0);
            }
        }
    }

    /// pid -> (parent pid, image name) at one instant.
    pub struct Snapshot(HashMap<u32, (u32, String)>);

    impl Snapshot {
        pub fn take() -> Option<Self> {
            let mut table = HashMap::new();
            // SAFETY: no pointers are passed; the returned handle is checked
            // before being wrapped in the single owner below.
            let raw = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
            if raw == INVALID_HANDLE_VALUE {
                return None;
            }
            let snapshot = OwnedHandle(raw);
            let mut entry = PROCESSENTRY32W {
                dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
                ..Default::default()
            };
            // SAFETY: `snapshot` is a live ToolHelp handle and `entry` points
            // to initialized writable storage with the required `dwSize`.
            let mut ok = unsafe { Process32FirstW(snapshot.0, &mut entry) };
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
                // SAFETY: same live handle and writable entry as above.
                ok = unsafe { Process32NextW(snapshot.0, &mut entry) };
            }
            Some(Self(table))
        }

        pub fn lookup(&self, pid: u32) -> Option<(u32, String)> {
            self.0.get(&pid).cloned()
        }

        /// Every process of the snapshot as (pid, image name).
        pub fn processes(self) -> Vec<(u32, String)> {
            self.0
                .into_iter()
                .map(|(pid, (_, name))| (pid, name))
                .collect()
        }
    }

    pub fn process_table(pid: u32) -> Option<ProcessTable> {
        let snapshot = Snapshot::take()?;
        let mut table = ProcessTable::default();
        let mut current = pid;
        let mut seen = Vec::new();
        while seen.len() < MAX_DEPTH && !seen.contains(&current) {
            seen.push(current);
            let Some((parent, name)) = snapshot.lookup(current) else {
                break;
            };
            // Access denied or an exited process makes this node unusable as
            // ancestry evidence, so the chain ends before it.
            let Some((image_path, created)) = query_process(current) else {
                break;
            };
            table.insert(
                current,
                ProcessEntry {
                    parent,
                    name,
                    image_path: Some(image_path),
                    created: Some(created),
                },
            );
            if parent == 0 || parent == current {
                break;
            }
            current = parent;
        }
        Some(table)
    }

    fn query_process(pid: u32) -> Option<(String, u64)> {
        // SAFETY: no pointers are passed. A null result is rejected before
        // the handle is wrapped and used.
        let raw = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
        if raw.is_null() {
            return None;
        }
        let process = OwnedHandle(raw);
        let mut path = vec![0u16; MAX_IMAGE_PATH];
        let mut path_len = path.len() as u32;
        let mut creation = FILETIME::default();
        let mut exit = FILETIME::default();
        let mut kernel = FILETIME::default();
        let mut user = FILETIME::default();
        // SAFETY: `process` is live; every pointer targets initialized,
        // writable storage for the duration of each call. `path_len` starts
        // at the actual UTF-16 buffer capacity.
        let (path_ok, times_ok) = unsafe {
            (
                QueryFullProcessImageNameW(
                    process.0,
                    PROCESS_NAME_WIN32,
                    path.as_mut_ptr(),
                    &mut path_len,
                ),
                GetProcessTimes(process.0, &mut creation, &mut exit, &mut kernel, &mut user),
            )
        };
        if path_ok == 0 || times_ok == 0 {
            return None;
        }
        path.truncate(path_len as usize);
        let created =
            (u64::from(creation.dwHighDateTime) << 32) | u64::from(creation.dwLowDateTime);
        Some((String::from_utf16_lossy(&path), created))
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use super::{MAX_DEPTH, Proc};

    /// Reads `/proc/<pid>/stat` on demand.
    pub fn ancestors(pid: u32) -> Vec<Proc> {
        let mut chain = Vec::new();
        let mut current = pid;
        while chain.len() < MAX_DEPTH {
            let Ok(stat) = std::fs::read_to_string(format!("/proc/{current}/stat")) else {
                break;
            };
            let Some((parent, name)) = super::parse_stat(&stat) else {
                break;
            };
            if chain.iter().any(|proc: &Proc| proc.pid == current) {
                break;
            }
            chain.push(Proc {
                pid: current,
                name,
                image_path: None,
            });
            if parent == 0 || parent == current {
                break;
            }
            current = parent;
        }
        chain
    }

    /// Every process in `/proc` as (pid, comm). `None` when `/proc` cannot
    /// be listed; a process that exits during the scan is skipped.
    pub fn processes() -> Option<Vec<(u32, String)>> {
        let mut out = Vec::new();
        for entry in std::fs::read_dir("/proc").ok()?.flatten() {
            let Some(pid) = entry
                .file_name()
                .to_str()
                .and_then(|name| name.parse::<u32>().ok())
            else {
                continue;
            };
            let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
                continue;
            };
            if let Some((_, name)) = super::parse_stat(&stat) {
                out.push((pid, name));
            }
        }
        Some(out)
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

    type ProcessRow<'a> = (u32, u32, &'a str, Option<&'a str>, Option<u64>);

    fn chain(nodes: &[(u32, &str)]) -> Vec<Proc> {
        nodes
            .iter()
            .map(|&(pid, name)| Proc {
                pid,
                name: name.to_owned(),
                image_path: None,
            })
            .collect()
    }

    fn process_table(nodes: &[ProcessRow<'_>]) -> ProcessTable {
        let mut table = ProcessTable::default();
        for &(pid, parent, name, image_path, created) in nodes {
            table.insert(
                pid,
                ProcessEntry {
                    parent,
                    name: name.to_owned(),
                    image_path: image_path.map(str::to_owned),
                    created,
                },
            );
        }
        table
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
        let mut nodes = chain(&[(1, "cctg.exe"), (2, "claude.exe"), (9, "claude.exe")]);
        assert_eq!(
            lineage(&nodes, Some(9), Some("parent-session"), SID),
            Lineage {
                claude_pid: Some(2),
                parent_claude_pid: Some(9)
            }
        );
        // A pid outside the verified chain is not enough to name a parent.
        nodes.pop();
        assert_eq!(
            lineage(&nodes, Some(9), Some("parent-session"), SID).parent_claude_pid,
            None
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
    fn desktop_images_are_not_cli_parents() {
        let own = Proc {
            pid: 2,
            name: "claude.exe".to_owned(),
            image_path: Some(
                r"C:\Users\u\AppData\Roaming\Claude\claude-code\2.1.280\claude.exe".to_owned(),
            ),
        };
        assert!(is_cli_claude(&own));
        for path in [
            r"C:\Program Files\WindowsApps\Claude_2.7032.0.0_x64__abc\app\claude.exe",
            r"C:\Users\u\AppData\Local\AnthropicClaude\app-1.0.0\claude.exe",
        ] {
            let nodes = vec![
                Proc {
                    pid: 1,
                    name: "cctg.exe".to_owned(),
                    image_path: Some(r"C:\bin\cctg.exe".to_owned()),
                },
                own.clone(),
                Proc {
                    pid: 3,
                    name: "claude.exe".to_owned(),
                    image_path: Some(path.to_owned()),
                },
            ];
            assert_eq!(
                lineage(&nodes, Some(2), Some(SID), SID),
                Lineage {
                    claude_pid: Some(2),
                    parent_claude_pid: None,
                },
                "{path}"
            );
        }
    }

    #[test]
    fn process_table_stops_at_reused_or_unqueryable_parent() {
        let valid = process_table(&[
            (1, 2, "cctg.exe", Some(r"C:\bin\cctg.exe"), Some(30)),
            (2, 3, "claude.exe", Some(r"C:\cli\claude.exe"), Some(20)),
            (3, 0, "claude.exe", Some(r"C:\cli\claude.exe"), Some(10)),
        ]);
        assert_eq!(
            valid
                .chain(1)
                .iter()
                .map(|proc| proc.pid)
                .collect::<Vec<_>>(),
            [1, 2, 3]
        );

        let reused = process_table(&[
            (1, 2, "cctg.exe", Some(r"C:\bin\cctg.exe"), Some(30)),
            // A parent created after its child means pid 2 was reused.
            (2, 3, "bash.exe", Some(r"C:\bin\bash.exe"), Some(40)),
            (3, 0, "claude.exe", Some(r"C:\cli\claude.exe"), Some(10)),
        ]);
        assert_eq!(
            reused
                .chain(1)
                .iter()
                .map(|proc| proc.pid)
                .collect::<Vec<_>>(),
            [1]
        );

        let denied = process_table(&[(1, 2, "cctg.exe", Some(r"C:\bin\cctg.exe"), Some(30))]);
        assert_eq!(
            denied
                .chain(1)
                .iter()
                .map(|proc| proc.pid)
                .collect::<Vec<_>>(),
            [1]
        );
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

    #[test]
    fn live_pids_are_claude_and_node_processes_only() {
        let processes = [
            (30, "claude.exe"),
            (7, "CLAUDE.EXE"),
            (12, "node.exe"),
            (5, "claude"),
            (8, "bash.exe"),
            (9, "claude-code.exe"),
            (10, "cctg.exe"),
            (7, "claude.exe"),
        ]
        .map(|(pid, name)| (pid, name.to_owned()));
        assert_eq!(claude_pids(processes), Some(vec![5, 7, 12, 30]));
        assert_eq!(claude_pids(Vec::new()), Some(Vec::new()));
        let many = (0..=MAX_LIVE_PIDS as u32).map(|pid| (pid, "claude.exe".to_owned()));
        assert_eq!(claude_pids(many), None, "an overlong list is no list");
    }

    #[cfg(any(windows, target_os = "linux"))]
    #[test]
    fn the_live_list_is_readable_here() {
        // Whatever runs here, the call works and stays bounded.
        let pids = live_claude_pids().expect("supported platform");
        assert!(pids.len() <= MAX_LIVE_PIDS);
    }

    #[cfg(any(windows, target_os = "linux"))]
    #[test]
    fn the_live_chain_starts_at_this_process() {
        let chain = ancestors(std::process::id()).expect("supported platform");
        assert_eq!(chain.first().map(|proc| proc.pid), Some(std::process::id()));
        assert!(chain.len() >= 2, "{chain:?}");
    }
}

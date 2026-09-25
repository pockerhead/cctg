//! `cctg agent`: the stdio shim between Claude Code and the worker agent
//! (TASK-040).
//!
//! Claude Code spawns `cctg agent` once per session and keeps its stdio for
//! the whole session. The shim starts `cctg agent-worker` (the channel
//! server, [`crate::agent::run_stdio`]) from the path it was itself started
//! from and copies lines both ways. When the worker wants a newer binary it
//! writes [`SWITCH`] on its stdout: the shim stops feeding it and closes its
//! stdin, the worker answers every line it already got and exits with
//! [`HANDOVER`], and the shim starts the file at the same path again (after
//! `cctg deploy` that is the new build) and gives it the lines that came
//! meanwhile. A worker that exits otherwise while Claude Code is still there
//! is started again after a second, at most [`MAX_CRASHES`] times a minute,
//! and from the same build: each worker runs from the shim's own hard link
//! to the file (in [`links_dir`]), made at the start and at each hand-over,
//! so a `cctg deploy` in between (which renames the file away and puts a new
//! one in its place) comes in only through ⬆️ Обновить. A link costs no copy
//! and Windows does not scan it again (a fresh copy took 0.5 s to start).
//! The worker finds the file its link came from in [`SOURCE_VAR`]. The shim
//! ends when Claude Code closes its stdin and the worker is gone.
//!
//! The shim itself is not updated while its session runs, so it knows
//! nothing of MCP, the hub or versions: lines, one marker, exit codes.

use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// The worker's exit code when it handed over to a newer binary.
pub const HANDOVER: i32 = 75;
/// The one line a worker writes to ask for the hand-over; never forwarded.
pub const SWITCH: &[u8] = b"{\"cctg_shim\":\"switch\"}\n";
/// What this shim can do, for the worker (env [`LEVEL_VAR`]).
pub const LEVEL: u32 = 1;
pub const LEVEL_VAR: &str = "CCTG_SHIM";
/// Unix seconds when the shim started (about when claude read its settings).
pub const STARTED_VAR: &str = "CCTG_SHIM_STARTED";
/// Set for every worker after the first: Claude Code initialized the channel
/// with an earlier one.
pub const RESUMED_VAR: &str = "CCTG_WORKER_RESUMED";
/// The file the worker's link was made from: where a new build shows up.
pub const SOURCE_VAR: &str = "CCTG_WORKER_SOURCE";
pub const MAX_CRASHES: usize = 3;
/// What the shim says on stderr (Claude Code's MCP log) when it gives up.
const GIVE_UP: &str = "cctg agent: the worker exited 4 times within a minute; \
the Telegram channel of this session is off until claude is started again";
const CRASH_WINDOW: Duration = Duration::from_secs(60);
const CRASH_WAIT: Duration = Duration::from_secs(1);

enum Event {
    /// A line from Claude Code; `None`: its stdin ended.
    Claude(Option<Vec<u8>>),
    /// The worker of generation `n` asked to hand over.
    Switch(u64),
    /// The worker of generation `n` closed its stdout.
    WorkerOut(u64),
}

/// Runs the shim; returns the process exit code.
pub fn run() -> i32 {
    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(_) => {
            eprintln!("cctg agent: cannot find this executable");
            return 1;
        }
    };
    let started = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs());
    let (events, inbox) = mpsc::channel();
    let claude = events.clone();
    std::thread::spawn(move || {
        let mut stdin = BufReader::new(std::io::stdin());
        loop {
            let mut line = Vec::new();
            match stdin.read_until(b'\n', &mut line) {
                Ok(0) | Err(_) => break,
                Ok(_) => {
                    if claude.send(Event::Claude(Some(line))).is_err() {
                        return;
                    }
                }
            }
        }
        let _ = claude.send(Event::Claude(None));
    });
    let links = links_dir(&exe);
    remove_dead_links(&links);
    serve(&exe, &links, started, &events, &inbox, std::io::stdout())
}

/// Where shims keep the links their workers run from: next to `exe`, on
/// its volume.
pub fn links_dir(exe: &Path) -> PathBuf {
    exe.parent()
        .unwrap_or_else(|| Path::new("."))
        .join("cctg-workers")
}

/// The file a worker runs from. A link of the shim's is removed with it.
struct Image {
    path: PathBuf,
    link: bool,
}

impl Image {
    /// A hard link `<shim pid>-<generation>-<name>` to `exe` in `dir`.
    fn link(exe: &Path, dir: &Path, generation: u64) -> std::io::Result<Self> {
        let mut name = std::ffi::OsString::from(format!("{}-{generation}-", std::process::id()));
        name.push(exe.file_name().unwrap_or_else(|| "cctg".as_ref()));
        std::fs::create_dir_all(dir)?;
        let path = dir.join(name);
        let _ = std::fs::remove_file(&path);
        std::fs::hard_link(exe, &path)?;
        Ok(Self { path, link: true })
    }

    fn file(exe: &Path) -> Self {
        Self {
            path: exe.to_owned(),
            link: false,
        }
    }
}

impl Drop for Image {
    fn drop(&mut self) {
        if self.link {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

/// Removes links of shims that are gone without cleaning up (a closed
/// window kills them): those whose shim pid no longer runs.
fn remove_dead_links(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(pid) = name
            .to_str()
            .and_then(|name| name.split('-').next())
            .and_then(|pid| pid.parse::<u32>().ok())
        else {
            continue;
        };
        let alive = crate::proctree::ancestors(pid)
            .is_none_or(|chain| chain.first().is_some_and(|proc| proc.pid == pid));
        if !alive {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// The shim loop over `inbox` (Claude Code lines from `events`' other end).
/// Worker stdout goes to `out`; worker links go into `links`.
fn serve<W: Write + Send + 'static>(
    exe: &Path,
    links: &Path,
    started: u64,
    events: &mpsc::Sender<Event>,
    inbox: &mpsc::Receiver<Event>,
    out: W,
) -> i32 {
    let out = std::sync::Arc::new(std::sync::Mutex::new(out));
    let mut waiting: VecDeque<Vec<u8>> = VecDeque::new();
    let mut claude_open = true;
    let mut crashes: VecDeque<Instant> = VecDeque::new();
    let mut generation = 0u64;
    // `None`: the next worker gets a new link (the first one, or after a
    // hand-over); a crashed worker's image is kept for the next.
    let mut image: Option<Image> = None;
    loop {
        let worker = |path: &Path| {
            let mut command = Command::new(path);
            command
                .arg("agent-worker")
                .env(LEVEL_VAR, LEVEL.to_string())
                .env(STARTED_VAR, started.to_string())
                .env(SOURCE_VAR, exe)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::inherit());
            if generation > 0 {
                command.env(RESUMED_VAR, "1");
            }
            #[cfg(windows)]
            {
                use std::os::windows::process::CommandExt;
                // No console window for the worker, whatever console we have.
                const CREATE_NO_WINDOW: u32 = 0x0800_0000;
                command.creation_flags(CREATE_NO_WINDOW);
            }
            command.spawn()
        };
        let (current, spawned) = match image.take() {
            Some(kept) => {
                let spawned = worker(&kept.path);
                (kept, spawned)
            }
            // No link (a file system without them): the file itself, as
            // before; a crash then restarts whatever the file holds.
            None => match Image::link(exe, links, generation) {
                Ok(link) => {
                    let spawned = worker(&link.path);
                    (link, spawned)
                }
                Err(_) => (Image::file(exe), worker(exe)),
            },
        };
        let mut child = match spawned {
            Ok(child) => child,
            Err(_) => {
                eprintln!("cctg agent: cannot start the worker");
                return 1;
            }
        };
        let mut to_worker = child.stdin.take();
        let from_worker = child.stdout.take();
        let (notify, out_copy, this) = (events.clone(), out.clone(), generation);
        std::thread::spawn(move || {
            if let Some(stdout) = from_worker {
                copy_worker_lines(stdout, &out_copy, &notify, this);
            }
            let _ = notify.send(Event::WorkerOut(this));
        });
        let mut switching = false;
        while let Some(line) = waiting.pop_front() {
            if !feed(&mut to_worker, &line) {
                waiting.push_front(line);
                break;
            }
        }
        if !claude_open {
            to_worker = None;
        }
        loop {
            match inbox.recv() {
                Ok(Event::Claude(Some(line))) => {
                    // After a switch `to_worker` is gone: the line waits,
                    // behind any that already wait.
                    if !waiting.is_empty() || !feed(&mut to_worker, &line) {
                        waiting.push_back(line);
                    }
                }
                Ok(Event::Claude(None)) => {
                    claude_open = false;
                    to_worker = None;
                }
                Ok(Event::Switch(from)) if from == generation => {
                    switching = true;
                    to_worker = None;
                }
                Ok(Event::WorkerOut(from)) if from == generation => break,
                Ok(_) => {}
                Err(_) => return 1,
            }
        }
        drop(to_worker);
        let code = child.wait().ok().and_then(|status| status.code());
        if !claude_open {
            return 0;
        }
        generation += 1;
        if switching && code == Some(HANDOVER) {
            // The old link goes; the next worker links the file anew.
            drop(current);
            continue;
        }
        image = Some(current);
        let now = Instant::now();
        crashes.retain(|at| now.duration_since(*at) < CRASH_WINDOW);
        crashes.push_back(now);
        if crashes.len() > MAX_CRASHES {
            eprintln!("{GIVE_UP}");
            return 1;
        }
        std::thread::sleep(CRASH_WAIT);
    }
}

/// Writes one line to the worker; `false` when it is gone (the line waits
/// for the next one).
fn feed(to_worker: &mut Option<std::process::ChildStdin>, line: &[u8]) -> bool {
    let Some(stdin) = to_worker else {
        return false;
    };
    if stdin.write_all(line).and_then(|()| stdin.flush()).is_ok() {
        return true;
    }
    *to_worker = None;
    false
}

/// Copies whole lines from the worker to `out`; the switch marker is not
/// copied but reported. A last line without its newline is dropped: a
/// half JSON-RPC line would break the transport.
fn copy_worker_lines<W: Write>(
    stdout: impl Read,
    out: &std::sync::Mutex<W>,
    notify: &mpsc::Sender<Event>,
    generation: u64,
) {
    let mut reader = BufReader::new(stdout);
    loop {
        let mut line = Vec::new();
        match reader.read_until(b'\n', &mut line) {
            Ok(0) | Err(_) => return,
            Ok(_) if line.last() != Some(&b'\n') => return,
            Ok(_) if line == SWITCH => {
                let _ = notify.send(Event::Switch(generation));
            }
            Ok(_) => {
                let mut out = out.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
                if out.write_all(&line).and_then(|()| out.flush()).is_err() {
                    return;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hub::testdir::TempDir;
    use std::sync::{Arc, Mutex};

    #[derive(Clone, Default)]
    struct Shared(Arc<Mutex<Vec<u8>>>);

    impl Write for Shared {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl Shared {
        /// How many times a worker ran: the stand-in prints this once.
        fn runs(&self) -> usize {
            String::from_utf8_lossy(&self.0.lock().unwrap())
                .matches("running 0 tests")
                .count()
        }
    }

    #[test]
    fn a_crashing_worker_restarts_from_its_own_build_and_the_shim_gives_up() {
        // The stand-in worker is this test binary: `agent-worker` is a test
        // filter that matches nothing, so it prints one summary and exits 0
        // at once, which for the shim is a crash.
        let dir = TempDir::new("shim-crash");
        let exe = dir
            .path()
            .join(format!("cctg-stand-in{}", std::env::consts::EXE_SUFFIX));
        std::fs::copy(std::env::current_exe().unwrap(), &exe).unwrap();
        let links = links_dir(&exe);
        let out = Shared::default();
        let (events, inbox) = mpsc::channel();
        let shim = std::thread::spawn({
            let (exe, links, out) = (exe.clone(), links.clone(), out.clone());
            move || {
                // `events` stays open: Claude Code is still there.
                let code = serve(&exe, &links, 0, &events, &inbox, out);
                (code, events)
            }
        });
        let deadline = Instant::now() + Duration::from_secs(60);
        while out.runs() == 0 {
            assert!(Instant::now() < deadline, "the first worker never ran");
            std::thread::sleep(Duration::from_millis(20));
        }
        // A deploy while the shim waits out its second (the file renamed
        // away, a new one in its place): the next worker must still be the
        // build the shim started with.
        std::fs::rename(&exe, dir.path().join("cctg-stand-in.old")).unwrap();
        std::fs::write(&exe, b"not a program").unwrap();
        let (code, _events) = shim.join().unwrap();
        assert_eq!(code, 1, "the shim gives up");
        assert_eq!(
            out.runs(),
            MAX_CRASHES + 1,
            "every restart ran the kept link"
        );
        let left: Vec<_> = std::fs::read_dir(&links).unwrap().flatten().collect();
        assert!(left.is_empty(), "the link goes with the shim: {left:?}");
    }

    // Needs a process source (`proctree::ancestors`); macOS has none yet
    // (TASK-044) and keeps every link.
    #[cfg(any(windows, target_os = "linux"))]
    #[test]
    fn only_links_of_shims_that_are_gone_are_removed() {
        let dir = TempDir::new("shim-links");
        let links = dir.path().join("cctg-workers");
        std::fs::create_dir_all(&links).unwrap();
        let mine = links.join(format!("{}-0-cctg.exe", std::process::id()));
        let gone = links.join("4000000000-3-cctg.exe");
        let other = links.join("notes.txt");
        for path in [&mine, &gone, &other] {
            std::fs::write(path, b"x").unwrap();
        }
        remove_dead_links(&links);
        assert!(mine.exists(), "a running shim keeps its link");
        assert!(!gone.exists(), "a gone shim's link is removed");
        assert!(other.exists(), "anything else is left alone");
    }
}

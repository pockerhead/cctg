//! The terminal `cctg run` keeps for its claude on Linux and macOS
//! (TASK-044), and the agent's way into it.
//!
//! On Windows the agent reaches its claude's console itself
//! ([`crate::keys`]). A Unix terminal has no such door, so `cctg run`, when
//! both its stdin and stdout are terminals, starts claude in a
//! pseudo-terminal of its own ([`Host`]): the user's terminal goes to raw
//! mode and bytes are relayed both ways unchanged (Ctrl+C, Ctrl+Z and every
//! other key reach claude as bytes), the window size follows the user's
//! terminal, and a copy of the screen is kept with a terminal emulator
//! ([`Screen`]). A socket `<state>/run/<cctg run pid>.sock` in a 0700
//! directory takes one [`Ask`] per connection: the visible rows, or text
//! written into claude's input. That is all `cctg run` knows: what the
//! screen shows is judged by the agent ([`crate::keys`]).
//!
//! The ask format is frozen: `cctg run` is not updated while it runs, the
//! agent is, so every later agent has to speak it. Asks are only added: an
//! older `cctg run` answers nothing to an ask it does not know, and the
//! agent falls back to one it does ([`rows`]).

use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

/// Longest ask line, in bytes (200 typed characters, JSON-escaped, fit).
pub const MAX_ASK: usize = 4096;

/// One ask of the agent: `"screen"`, `"rows"` (TASK-057) or
/// `{"keys":"<text>"}`. The answer is one JSON line: the rows (`null` when
/// there is no copy), the [`Rows`] (`null` likewise) or whether the bytes
/// were written.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Ask {
    Screen,
    Keys(String),
    Rows,
}

/// One read of a screen: the visible rows, right trimmed, and, when the
/// reader can tell, the same rows with faint (SGR 2) cells as spaces.
/// Claude Code draws its placeholder, its prompt suggestion and the inline
/// completion after the cursor faint (probe TASK-057, 2.1.283): text in the
/// input box that nobody typed. A Windows console read cannot tell (its
/// attribute words carry no faint), so there `solid` is `None`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rows {
    pub lines: Vec<String>,
    #[serde(default)]
    pub solid: Option<Vec<String>>,
}

/// `<state>/run/<run_pid>.sock`.
pub fn socket_path(state_dir: &Path, run_pid: u32) -> PathBuf {
    state_dir.join("run").join(format!("{run_pid}.sock"))
}

/// The screen copy: a terminal emulator fed with everything claude writes.
pub struct Screen {
    /// `None` after the emulator panicked on some output: no copy anymore,
    /// the relay goes on.
    parser: Option<vt100::Parser>,
}

impl Screen {
    pub fn new(rows: u16, cols: u16) -> Self {
        Self {
            parser: Some(vt100::Parser::new(rows.max(1), cols.max(1), 0)),
        }
    }

    pub fn feed(&mut self, bytes: &[u8]) {
        let Some(parser) = self.parser.as_mut() else {
            return;
        };
        let fed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| parser.process(bytes)));
        if fed.is_err() {
            self.parser = None;
        }
    }

    pub fn resize(&mut self, rows: u16, cols: u16) {
        if let Some(parser) = self.parser.as_mut() {
            parser.screen_mut().set_size(rows.max(1), cols.max(1));
        }
    }

    /// The visible rows, right trimmed, like the Windows console read.
    pub fn lines(&self) -> Option<Vec<String>> {
        let screen = self.parser.as_ref()?.screen();
        let (_, cols) = screen.size();
        Some(
            screen
                .rows(0, cols)
                .map(|row| row.trim_end().to_owned())
                .collect(),
        )
    }

    /// [`Screen::lines`] and the same rows without their faint text.
    pub fn rows(&self) -> Option<Rows> {
        let screen = self.parser.as_ref()?.screen();
        let (rows, cols) = screen.size();
        let solid = (0..rows)
            .map(|row| {
                let mut line = String::new();
                for col in 0..cols {
                    match screen.cell(row, col) {
                        Some(cell) if cell.is_wide_continuation() => {}
                        Some(cell) if cell.has_contents() && !cell.dim() => {
                            line.push_str(cell.contents())
                        }
                        _ => line.push(' '),
                    }
                }
                line.trim_end().to_owned()
            })
            .collect();
        Some(Rows {
            lines: self.lines()?,
            solid: Some(solid),
        })
    }
}

/// Answers one [`Ask`] read from `stream`: rows from `screen`, keys through
/// `write`. A line that is too long or no ask gets no answer.
pub fn answer<S: Read + Write>(
    mut stream: S,
    screen: &Mutex<Screen>,
    write: &dyn Fn(&[u8]) -> bool,
) -> std::io::Result<()> {
    let mut line = Vec::new();
    BufReader::new((&mut stream).take(MAX_ASK as u64 + 1)).read_until(b'\n', &mut line)?;
    if line.len() > MAX_ASK {
        return Ok(());
    }
    let Ok(ask) = serde_json::from_slice::<Ask>(&line) else {
        return Ok(());
    };
    let mut reply = match ask {
        Ask::Screen => {
            let lines = screen
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .lines();
            serde_json::to_vec(&lines)?
        }
        Ask::Rows => {
            let rows = screen
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .rows();
            serde_json::to_vec(&rows)?
        }
        Ask::Keys(text) => serde_json::to_vec(&write(text.as_bytes()))?,
    };
    reply.push(b'\n');
    stream.write_all(&reply)?;
    stream.flush()
}

/// The visible rows of the `cctg run` terminal behind `socket`; `None`
/// when it does not answer.
pub fn screen(socket: &Path) -> Option<Vec<String>> {
    serde_json::from_slice::<Option<Vec<String>>>(&ask(socket, &Ask::Screen)?)
        .ok()
        .flatten()
}

/// The [`Rows`] of the `cctg run` terminal behind `socket`; from a
/// `cctg run` older than the `rows` ask, its visible rows without `solid`.
/// `None` when it does not answer.
pub fn rows(socket: &Path) -> Option<Rows> {
    ask(socket, &Ask::Rows)
        .and_then(|reply| serde_json::from_slice::<Option<Rows>>(&reply).ok())
        .flatten()
        .or_else(|| screen(socket).map(|lines| Rows { lines, solid: None }))
}

/// Writes `text` into the input of the claude behind `socket`.
pub fn keys(socket: &Path, text: &str) -> bool {
    ask(socket, &Ask::Keys(text.to_owned()))
        .and_then(|reply| serde_json::from_slice::<bool>(&reply).ok())
        .unwrap_or(false)
}

#[cfg(unix)]
fn ask(socket: &Path, ask: &Ask) -> Option<Vec<u8>> {
    use std::os::unix::net::UnixStream;
    const WAIT: std::time::Duration = std::time::Duration::from_secs(2);

    let mut stream = UnixStream::connect(socket).ok()?;
    stream.set_read_timeout(Some(WAIT)).ok()?;
    stream.set_write_timeout(Some(WAIT)).ok()?;
    let mut line = serde_json::to_vec(ask).ok()?;
    line.push(b'\n');
    stream.write_all(&line).ok()?;
    let mut reply = Vec::new();
    BufReader::new(stream.take(1 << 20))
        .read_until(b'\n', &mut reply)
        .ok()?;
    Some(reply)
}

#[cfg(not(unix))]
fn ask(_socket: &Path, _ask: &Ask) -> Option<Vec<u8>> {
    None
}

#[cfg(unix)]
pub use host::Host;

#[cfg(unix)]
mod host {
    use std::fs::File;
    use std::io::{Read, Write};
    use std::os::fd::{AsRawFd, FromRawFd};
    use std::os::unix::process::CommandExt;
    use std::path::{Path, PathBuf};
    use std::process::Command;
    use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
    use std::sync::{Arc, Mutex};

    use super::Screen;

    /// The terminal side of one `cctg run`, shared by its threads. Every
    /// claude gets a pseudo-terminal of its own as its controlling terminal:
    /// macOS revokes a controlling terminal when its session leader exits,
    /// so one cannot serve the next claude.
    pub struct Host {
        /// The master of the current claude's pseudo-terminal: keys go in
        /// here (the user's, the agent's, the channels dialog's Enter), one
        /// write at a time.
        master: Mutex<Option<File>>,
        /// One copy for every claude of this run, like a console that keeps
        /// the frame of the claude that exited.
        screen: Arc<Mutex<Screen>>,
        /// The claude running now (its process group), 0 between runs.
        claude: AtomicI32,
        /// SIGTERM or SIGHUP came: no restart.
        stopping: AtomicBool,
        /// The user's terminal mode before raw mode.
        saved: libc::termios,
        /// The socket, once bound.
        socket: Mutex<Option<PathBuf>>,
    }

    fn window_size(fd: i32) -> libc::winsize {
        let mut size = libc::winsize {
            ws_row: 0,
            ws_col: 0,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        // SAFETY: a live winsize for the ioctl to fill.
        let ok = unsafe { libc::ioctl(fd, libc::TIOCGWINSZ as _, &mut size as *mut libc::winsize) };
        if ok != 0 || size.ws_row == 0 || size.ws_col == 0 {
            size.ws_row = 24;
            size.ws_col = 80;
        }
        size
    }

    fn close_on_exec(fd: i32) {
        // SAFETY: plain fcntl on a descriptor this process owns.
        unsafe {
            libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC);
        }
    }

    impl Host {
        /// The user's terminal in raw mode, its keys relayed, the socket
        /// served. `None` unless stdin and stdout are terminals.
        /// [`Host::finish`] undoes it.
        pub fn start(socket: Option<PathBuf>) -> Option<Arc<Host>> {
            // SAFETY: isatty takes any descriptor.
            if unsafe { libc::isatty(0) == 0 || libc::isatty(1) == 0 } {
                return None;
            }
            // SAFETY: zeroed termios is valid plain data; tcgetattr fills it.
            let mut saved: libc::termios = unsafe { std::mem::zeroed() };
            if unsafe { libc::tcgetattr(0, &mut saved) } != 0 {
                return None;
            }
            let size = window_size(0);
            let host = Arc::new(Host {
                master: Mutex::new(None),
                screen: Arc::new(Mutex::new(Screen::new(size.ws_row, size.ws_col))),
                claude: AtomicI32::new(0),
                stopping: AtomicBool::new(false),
                saved,
                socket: Mutex::new(None),
            });
            if let Some(path) = socket
                && host.serve(&path)
            {
                *host.socket.lock().unwrap_or_else(|p| p.into_inner()) = Some(path);
            }
            host.raw();
            std::thread::spawn({
                let host = host.clone();
                move || relay_input(&host)
            });
            Some(host)
        }

        /// Back to the terminal mode `cctg run` found.
        fn restore(&self) {
            // SAFETY: fd 0 and a termios read from it.
            unsafe {
                libc::tcsetattr(0, libc::TCSANOW, &self.saved);
            }
        }

        /// Raw mode: every key goes to claude as it is typed.
        fn raw(&self) {
            let mut raw = self.saved;
            // SAFETY: a live termios copy; fd 0.
            unsafe {
                libc::cfmakeraw(&mut raw);
                libc::tcsetattr(0, libc::TCSANOW, &raw);
            }
        }

        /// When `cctg run` ends: claude's last output gets a moment to reach
        /// the terminal, then the terminal mode is restored and the socket
        /// removed.
        pub fn finish(&self) {
            std::thread::sleep(std::time::Duration::from_millis(100));
            self.restore();
            if let Some(socket) = self.socket.lock().unwrap_or_else(|p| p.into_inner()).take() {
                let _ = std::fs::remove_file(socket);
            }
        }

        /// Writes `bytes` into the current claude's input.
        pub fn write(&self, bytes: &[u8]) -> bool {
            let mut master = self.master.lock().unwrap_or_else(|p| p.into_inner());
            master
                .as_mut()
                .is_some_and(|master| master.write_all(bytes).is_ok())
        }

        pub fn lines(&self) -> Option<Vec<String>> {
            self.screen
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .lines()
        }

        /// The pseudo-terminal takes the user's window size (the kernel
        /// tells claude with SIGWINCH).
        pub fn sync_size(&self) {
            let size = window_size(0);
            if let Some(master) = self
                .master
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .as_ref()
            {
                // SAFETY: a live winsize; the master descriptor is open.
                unsafe {
                    libc::ioctl(
                        master.as_raw_fd(),
                        libc::TIOCSWINSZ as _,
                        &size as *const libc::winsize,
                    );
                }
            }
            self.screen
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .resize(size.ws_row, size.ws_col);
        }

        /// `program` in a new pseudo-terminal of the user's terminal mode
        /// and size: its stdin, stdout, stderr and controlling terminal, in
        /// a session of its own. Its output is relayed from now on.
        pub fn command(&self, program: &std::ffi::OsStr) -> std::io::Result<Command> {
            let mut modes = self.saved;
            let mut size = window_size(0);
            let (mut master, mut slave) = (-1, -1);
            // SAFETY: out pointers to live locals; the pseudo-terminal
            // starts with the user's terminal mode and size, as `script`'s.
            let opened = unsafe {
                libc::openpty(
                    &mut master,
                    &mut slave,
                    std::ptr::null_mut(),
                    &raw mut modes,
                    &raw mut size,
                )
            };
            if opened != 0 {
                return Err(std::io::Error::last_os_error());
            }
            // Neither end may leak into claude's children: a leaked master
            // keeps the terminal open after `cctg run` is gone.
            close_on_exec(master);
            close_on_exec(slave);
            // SAFETY: openpty returned two fresh descriptors owned here.
            let (master, slave) = unsafe { (File::from_raw_fd(master), File::from_raw_fd(slave)) };
            let reader = master.try_clone()?;
            let mut command = Command::new(program);
            command
                .stdin(slave.try_clone()?)
                .stdout(slave.try_clone()?)
                .stderr(slave);
            // SAFETY: only async-signal-safe calls between fork and exec.
            unsafe {
                command.pre_exec(|| {
                    if libc::setsid() == -1 || libc::ioctl(0, libc::TIOCSCTTY as _, 0) == -1 {
                        return Err(std::io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
            self.screen
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .resize(size.ws_row, size.ws_col);
            *self.master.lock().unwrap_or_else(|p| p.into_inner()) = Some(master);
            std::thread::spawn({
                let screen = self.screen.clone();
                move || relay_output(reader, &screen)
            });
            Ok(command)
        }

        /// Waits for claude `pid` to exit; its exit code (128 + signal when
        /// a signal ended it). When claude stops itself (Ctrl+Z: Claude Code
        /// sends itself SIGSTOP), this process stops the same way, the
        /// user's terminal back in its own mode, and both go on together
        /// after `fg`. SIGSTOP, not SIGTSTP: claude's process group has no
        /// parent in its session, so the kernel would drop a SIGTSTP.
        pub fn wait(&self, pid: u32) -> i32 {
            let pid = pid as libc::pid_t;
            self.claude.store(pid, Ordering::SeqCst);
            if self.stopping() {
                // SIGTERM or SIGHUP came while this claude was being
                // started: [`Host::hang_up`] found no claude to tell.
                // SAFETY: a signal to claude's own process group.
                unsafe {
                    libc::kill(-pid, libc::SIGHUP);
                }
            }
            let code = loop {
                let mut status = 0;
                // SAFETY: waits for this process's own child.
                if unsafe { libc::waitpid(pid, &mut status, libc::WUNTRACED) } == -1 {
                    if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                        continue;
                    }
                    break 1;
                }
                if libc::WIFEXITED(status) {
                    break libc::WEXITSTATUS(status);
                }
                if libc::WIFSIGNALED(status) {
                    break 128 + libc::WTERMSIG(status);
                }
                if libc::WIFSTOPPED(status) {
                    // What claude wrote before it stopped (its terminal
                    // modes undone, the "suspended" line) reaches the
                    // terminal before the shell takes it back.
                    std::thread::sleep(std::time::Duration::from_millis(100));
                    self.restore();
                    // To this thread, which stops before it returns (the
                    // whole process stops with it). `kill(getpid())` lets
                    // Linux hand the stop to the main thread while this
                    // one runs on into raw mode.
                    // SAFETY: a signal to this thread.
                    unsafe {
                        libc::raise(libc::SIGSTOP);
                    }
                    self.raw();
                    self.sync_size();
                    // SAFETY: a signal to claude's own process group.
                    unsafe {
                        libc::kill(-pid, libc::SIGCONT);
                    }
                }
            };
            self.claude.store(0, Ordering::SeqCst);
            code
        }

        /// SIGTERM or SIGHUP for `cctg run`: claude gets SIGHUP, as when its
        /// terminal closes, and is not started again.
        pub fn hang_up(&self) {
            self.stopping.store(true, Ordering::SeqCst);
            let pid = self.claude.load(Ordering::SeqCst);
            if pid > 0 {
                // SAFETY: a signal to claude's own process group.
                unsafe {
                    libc::kill(-pid, libc::SIGHUP);
                }
            }
        }

        pub fn stopping(&self) -> bool {
            self.stopping.load(Ordering::SeqCst)
        }

        /// Binds the socket (directory 0700, socket 0600) and answers asks
        /// on a thread; `false` when it cannot be bound.
        fn serve(self: &Arc<Self>, path: &Path) -> bool {
            use std::os::unix::fs::PermissionsExt;
            use std::os::unix::net::UnixListener;

            let Some(dir) = path.parent() else {
                return false;
            };
            if std::fs::create_dir_all(dir).is_err()
                || std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700)).is_err()
            {
                return false;
            }
            let _ = std::fs::remove_file(path);
            let listener = match UnixListener::bind(path) {
                Ok(listener) => listener,
                Err(error) => {
                    eprintln!("cctg run: no terminal socket for cctg: {error}");
                    return false;
                }
            };
            let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
            let host = self.clone();
            std::thread::spawn(move || {
                for stream in listener.incoming() {
                    // A failing accept (no descriptors left) is retried
                    // later, not in a busy loop.
                    let Ok(stream) = stream else {
                        std::thread::sleep(std::time::Duration::from_millis(100));
                        continue;
                    };
                    let wait = Some(std::time::Duration::from_secs(2));
                    let _ = stream.set_read_timeout(wait);
                    let _ = stream.set_write_timeout(wait);
                    let _ = super::answer(stream, &host.screen, &|bytes| host.write(bytes));
                }
            });
            true
        }
    }

    /// One claude's output to the user's terminal and into the screen copy,
    /// until its pseudo-terminal has no process left (EOF on macOS, EIO on
    /// Linux).
    fn relay_output(mut master: File, screen: &Mutex<Screen>) {
        let mut buf = [0u8; 8192];
        let mut out = std::io::stdout();
        loop {
            let n = match master.read(&mut buf) {
                Ok(0) => return,
                Ok(n) => n,
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => return,
            };
            let _ = out.write_all(&buf[..n]);
            let _ = out.flush();
            screen
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .feed(&buf[..n]);
        }
    }

    /// The user's keys to the current claude, as they come.
    fn relay_input(host: &Host) {
        let mut buf = [0u8; 4096];
        let mut input = std::io::stdin();
        loop {
            match input.read(&mut buf) {
                Ok(0) => return,
                Ok(n) => {
                    host.write(&buf[..n]);
                }
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                Err(_) => return,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One ask in, the answer out, in memory.
    struct Duplex {
        input: std::io::Cursor<Vec<u8>>,
        output: Vec<u8>,
    }

    impl Read for Duplex {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            self.input.read(buf)
        }
    }

    impl Write for Duplex {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.output.write(buf)
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    fn ask(line: &[u8], screen: &Mutex<Screen>, written: &Mutex<Vec<u8>>) -> String {
        let mut duplex = Duplex {
            input: std::io::Cursor::new(line.to_vec()),
            output: Vec::new(),
        };
        answer(&mut duplex, screen, &|bytes| {
            written.lock().unwrap().extend_from_slice(bytes);
            true
        })
        .unwrap();
        String::from_utf8(duplex.output).unwrap()
    }

    #[test]
    fn the_ask_format_is_frozen() {
        assert_eq!(serde_json::to_string(&Ask::Screen).unwrap(), r#""screen""#);
        assert_eq!(
            serde_json::to_string(&Ask::Keys("\u{1b}".into())).unwrap(),
            r#"{"keys":"\u001b"}"#
        );
        assert_eq!(serde_json::to_string(&Ask::Rows).unwrap(), r#""rows""#);
    }

    /// The input box as Claude Code 2.1.283 drew it (probe TASK-057,
    /// `scratch/probe/conpty.bin`): rules in `promptBorder` grey, the
    /// placeholder faint after the glyph and its no-break space.
    const PLACEHOLDER: &str = "\x1b[38;2;136;136;136m────────────────────\x1b[m\r\n\
        ❯\u{a0}\x1b[2mTry\x1b[1C\"how\x1b[1Cdo\x1b[1CI\x1b[1Clog\x1b[1Can\x1b[1Cerror?\"\
        \x1b[38;2;136;136;136m\x1b[22m\r\n────────────────────\x1b[m";

    #[test]
    fn faint_text_is_left_out_of_the_solid_rows() {
        let mut screen = Screen::new(4, 40);
        screen.feed(PLACEHOLDER.as_bytes());
        let rows = screen.rows().unwrap();
        assert_eq!(rows.lines[1], "❯\u{a0}Try \"how do I log an error?\"");
        // Right trimmed like the lines: the no-break space goes too.
        let rule = "─".repeat(20);
        assert_eq!(
            rows.solid.as_deref(),
            Some(&[rule.clone(), "❯".to_owned(), rule, String::new()][..])
        );
        assert_eq!(Some(rows.lines), screen.lines());
        // Typed text replaces the placeholder; an inline completion after
        // it is faint again; a wide character keeps its place.
        screen.feed("\x1b[2;3H!ls 日本\x1b[2m -la\x1b[22m\x1b[K".as_bytes());
        let rows = screen.rows().unwrap();
        assert_eq!(rows.lines[1], "❯\u{a0}!ls 日本 -la");
        assert_eq!(rows.solid.as_ref().unwrap()[1], "❯\u{a0}!ls 日本");
        let typed = crate::keys::typed_box(&rows).unwrap();
        assert!(crate::keys::box_shows(&typed, "!ls 日本"));
        let whole = crate::keys::input_box(&rows.lines).unwrap();
        assert!(!crate::keys::box_shows(&whole, "!ls 日本"));
    }

    #[test]
    fn asks_are_answered_one_line_each() {
        let screen = Mutex::new(Screen::new(4, 20));
        screen.lock().unwrap().feed("❯\u{a0}/cost\r\n".as_bytes());
        let written = Mutex::new(Vec::new());
        assert_eq!(
            ask(b"\"screen\"\n", &screen, &written),
            "[\"❯\u{a0}/cost\",\"\",\"\",\"\"]\n"
        );
        assert_eq!(
            ask(b"\"rows\"\n", &screen, &written),
            "{\"lines\":[\"❯\u{a0}/cost\",\"\",\"\",\"\"],\
             \"solid\":[\"❯\u{a0}/cost\",\"\",\"\",\"\"]}\n"
        );
        assert_eq!(ask(br#"{"keys":"/x\r"}"#, &screen, &written), "true\n");
        assert_eq!(written.lock().unwrap().as_slice(), b"/x\r");
        // Garbage and overlong asks get nothing, nothing is written.
        for bad in [&b"\"typo\"\n"[..], &b"{}"[..], &b""[..]] {
            assert_eq!(ask(bad, &screen, &written), "", "{bad:?}");
        }
        let long = format!("{{\"keys\":\"{}\"}}\n", "x".repeat(MAX_ASK));
        assert_eq!(ask(long.as_bytes(), &screen, &written), "");
        assert_eq!(written.lock().unwrap().len(), 3);
    }

    #[test]
    fn the_copy_follows_redraws_like_the_terminal() {
        let mut screen = Screen::new(5, 40);
        // An Ink-like frame, then a redraw of the box in place: cursor up
        // a line, erase it, new content.
        screen
            .feed("hello\r\n────────────────────\r\n❯\u{a0}dra\r\n────────────────────".as_bytes());
        screen.feed("\x1b[1A\r\x1b[2K❯\u{a0}/cost\x1b[1B\r".as_bytes());
        let lines = screen.lines().unwrap();
        assert_eq!(
            lines,
            [
                "hello",
                "────────────────────",
                "❯\u{a0}/cost",
                "────────────────────",
                ""
            ]
        );
        let found = crate::keys::input_box(&lines).unwrap();
        assert!(crate::keys::box_shows(&found, "/cost"));
        // Wide characters and a resize keep working.
        screen.feed("\x1b[5;1H日本".as_bytes());
        screen.resize(6, 30);
        assert_eq!(screen.lines().unwrap()[4], "日本");
        assert_eq!(screen.lines().unwrap().len(), 6);
    }

    #[test]
    fn the_copy_can_be_shared_by_the_relay_threads() {
        fn shared<T: Send + Sync>() {}
        shared::<Mutex<Screen>>();
    }

    #[test]
    fn the_socket_is_named_after_the_run() {
        assert!(socket_path(Path::new("/s"), 42).ends_with("run/42.sock"));
    }

    /// A `cctg run` from before the `rows` ask: it answers `"screen"` and
    /// closes any other ask unanswered. The agent still gets the rows.
    #[cfg(unix)]
    #[test]
    fn an_old_run_still_gives_its_rows() {
        use std::os::unix::net::UnixListener;

        let dir = std::env::temp_dir().join(format!("cctg-old-run-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("1.sock");
        let _ = std::fs::remove_file(&path);
        let listener = UnixListener::bind(&path).unwrap();
        let old = std::thread::spawn(move || {
            let mut asks = Vec::new();
            for _ in 0..2 {
                let (mut stream, _) = listener.accept().unwrap();
                let mut line = Vec::new();
                BufReader::new(&mut stream)
                    .read_until(b'\n', &mut line)
                    .unwrap();
                if line == b"\"screen\"\n" {
                    stream.write_all(b"[\"\xe2\x9d\xaf x\"]\n").unwrap();
                }
                asks.push(String::from_utf8(line).unwrap());
            }
            asks
        });
        assert_eq!(
            rows(&path),
            Some(Rows {
                lines: vec!["❯ x".to_owned()],
                solid: None,
            })
        );
        assert_eq!(old.join().unwrap(), ["\"rows\"\n", "\"screen\"\n"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(not(unix))]
    #[test]
    fn without_unix_sockets_nothing_answers() {
        let path = Path::new("nowhere.sock");
        assert_eq!(screen(path), None);
        assert_eq!(rows(path), None);
        assert!(!keys(path, "x"));
    }
}

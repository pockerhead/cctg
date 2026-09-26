//! `cctg run` in a terminal on Linux and macOS (TASK-044), with real
//! processes. This test binary is the user's terminal (a pseudo-terminal it
//! opens, `cctg run` its session leader) and the claude stand-in
//! (`RUN_PTY_ROLE=claude`) that `cctg run` starts in its own
//! pseudo-terminal. The stand-in draws the development channels dialog and
//! then an input box that echoes keys; `/cost` opens a panel, `/view` the
//! agent view, `/busy` makes `/exit` open the background-work dialog, Esc
//! closes any of them, Ctrl+Z stops it with SIGSTOP as Claude Code does,
//! `/exit` ends it (code 7 once resumed). The test drives it the way the
//! agent does, through `cctg::keys` on the `cctg run` socket, and the way
//! the user does, through its terminal. No window, temp home and state.

#[cfg(unix)]
mod common;

fn main() {
    #[cfg(unix)]
    {
        if std::env::var("RUN_PTY_ROLE").as_deref() == Ok("claude") {
            unix::fake_claude();
        }
        if std::env::args().any(|arg| arg == "--list") {
            println!("run_pty_e2e: test");
            return;
        }
        unix::scenario();
        println!("run_pty_e2e: ok");
    }
    #[cfg(not(unix))]
    println!("run_pty_e2e: Linux and macOS only");
}

#[cfg(unix)]
mod unix {
    use std::fs::File;
    use std::io::{Read, Write};
    use std::os::fd::{AsRawFd, FromRawFd};
    use std::os::unix::process::CommandExt;
    use std::path::PathBuf;
    use std::process::Stdio;
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    use cctg::keys::{self, Target, Typed};
    use cctg::update::Worker;
    use cctg::wire::ConsoleKey;
    use serde_json::{Value, json};

    const SESSION: &str = "5e550000-0000-4000-8000-000000000pty";
    const RULE: &str = "────────────────────────────────────────";
    const WAIT: Duration = Duration::from_secs(20);
    const SUSPENDED: &str = "fake claude suspended; fg to resume";

    fn log(event: Value) {
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(std::env::var("RUN_PTY_LOG").unwrap())
            .unwrap();
        writeln!(file, "{event}").unwrap();
    }

    fn size_of(fd: i32) -> (u16, u16) {
        let mut size = libc::winsize {
            ws_row: 0,
            ws_col: 0,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        unsafe { libc::ioctl(fd, libc::TIOCGWINSZ as _, &mut size as *mut libc::winsize) };
        (size.ws_row, size.ws_col)
    }

    fn draw(lines: &[String]) {
        let mut out = std::io::stdout();
        write!(out, "\x1b[H\x1b[2J{}", lines.join("\r\n")).unwrap();
        out.flush().unwrap();
    }

    /// The claude stand-in.
    pub fn fake_claude() -> ! {
        let args: Vec<String> = std::env::args().skip(1).collect();
        let resumed = args.iter().any(|arg| arg == "--resume");
        let mut modes: libc::termios = unsafe { std::mem::zeroed() };
        unsafe {
            libc::tcgetattr(0, &mut modes);
            libc::cfmakeraw(&mut modes);
            libc::tcsetattr(0, libc::TCSANOW, &modes);
        }
        log(json!({
            "start": args,
            "run": std::env::var("CCTG_RUN").unwrap_or_default(),
            "size": size_of(0),
        }));
        draw(
            &[
                " WARNING: Loading development channels",
                "",
                "  Channels: server:cctg",
                "",
                "  \u{276f} 1. I am using this for local development",
                "    2. Exit",
                "",
                "  Enter to confirm \u{b7} Esc to cancel",
            ]
            .map(str::to_owned),
        );
        let mut stdin = std::io::stdin();
        let mut buf = [0u8; 1024];
        loop {
            let n = stdin.read(&mut buf).unwrap();
            if n == 0 {
                std::process::exit(1);
            }
            if buf[..n].contains(&b'\r') {
                break;
            }
        }
        log(json!({ "dialog": true }));
        let (mut input, mut panel, mut view, mut busy, mut dialog) =
            (String::new(), false, false, false, false);
        let mut size = size_of(0);
        loop {
            let mut lines = vec!["\u{25cf} ready".to_owned()];
            if panel {
                lines.push("\u{2594}".repeat(40));
                lines.push("   Session".into());
                lines.push("   Total cost: $0".into());
            } else if dialog {
                lines.push("\u{2594}".repeat(40));
                lines.push("   Background work is running".into());
                lines.push("   \u{276f} 1. Exit and stop tasks".into());
                lines.push("     3. Stay".into());
            } else {
                let shown = if view && input.is_empty() {
                    "Message @qa\u{2026}".to_owned()
                } else {
                    input.clone()
                };
                lines.push(RULE.into());
                lines.push(format!("\u{276f}\u{a0}{shown}"));
                lines.push(RULE.into());
            }
            draw(&lines);
            log(json!({ "drawn": input }));
            let n = stdin.read(&mut buf).unwrap();
            if n == 0 {
                std::process::exit(1);
            }
            if size_of(0) != size {
                size = size_of(0);
                log(json!({ "size": size }));
            }
            for c in String::from_utf8_lossy(&buf[..n]).chars() {
                match c {
                    '\r' => {
                        let line = std::mem::take(&mut input);
                        log(json!({ "submit": line }));
                        match line.as_str() {
                            "/cost" => panel = true,
                            "/view" => view = true,
                            "/busy" => busy = true,
                            "/idle" => busy = false,
                            "/exit" if busy => dialog = true,
                            "/exit" => {
                                log(json!({ "exit": true }));
                                std::process::exit(if resumed { 7 } else { 0 });
                            }
                            _ => {}
                        }
                    }
                    '\u{7f}' => {
                        input.pop();
                    }
                    '\u{1b}' => {
                        log(json!({ "esc": true }));
                        (panel, view, dialog) = (false, false, false);
                    }
                    '\u{3}' => log(json!({ "ctrl_c": true })),
                    '\u{1a}' => {
                        log(json!({ "suspend": true }));
                        // Like Claude Code: a last line, then SIGSTOP.
                        print!("{SUSPENDED}\r\n");
                        std::io::stdout().flush().unwrap();
                        unsafe { libc::kill(libc::getpid(), libc::SIGSTOP) };
                        log(json!({ "resumed": true }));
                    }
                    c => input.push(c),
                }
            }
        }
    }

    /// The user's side: the terminal `cctg run` runs in.
    struct Terminal {
        master: File,
        slave: File,
        output: Arc<Mutex<Vec<u8>>>,
    }

    impl Terminal {
        fn open() -> Self {
            let (mut master, mut slave) = (-1, -1);
            let mut size = libc::winsize {
                ws_row: 30,
                ws_col: 100,
                ws_xpixel: 0,
                ws_ypixel: 0,
            };
            let opened = unsafe {
                libc::openpty(
                    &mut master,
                    &mut slave,
                    std::ptr::null_mut(),
                    std::ptr::null_mut::<libc::termios>(),
                    &raw mut size,
                )
            };
            assert_eq!(opened, 0, "openpty: {}", std::io::Error::last_os_error());
            let (master, slave) = unsafe { (File::from_raw_fd(master), File::from_raw_fd(slave)) };
            // Someone has to read the terminal, or `cctg run` blocks on it.
            let output = Arc::new(Mutex::new(Vec::new()));
            let mut reader = master.try_clone().unwrap();
            let sink = output.clone();
            std::thread::spawn(move || {
                let mut buf = [0u8; 8192];
                while let Ok(n) = reader.read(&mut buf) {
                    if n == 0 {
                        break;
                    }
                    sink.lock().unwrap().extend_from_slice(&buf[..n]);
                }
            });
            Self {
                master,
                slave,
                output,
            }
        }

        fn type_keys(&self, bytes: &[u8]) {
            (&self.master).write_all(bytes).unwrap();
        }

        /// Asserts that raw mode (no line editing, no echo) is on or off.
        /// The mode is read through the master: macOS revokes the slave
        /// when its session leader, `cctg run`, exits.
        fn expect_raw(&self, raw: bool, what: &str) {
            let mut modes: libc::termios = unsafe { std::mem::zeroed() };
            let got = unsafe { libc::tcgetattr(self.master.as_raw_fd(), &mut modes) };
            let error = std::io::Error::last_os_error();
            assert_eq!(got, 0, "{what}: tcgetattr: {error}");
            let (lflag, cooked) = (modes.c_lflag, libc::ICANON | libc::ECHO);
            assert_eq!(
                lflag & cooked == 0,
                raw,
                "{what}: c_lflag {lflag:#x}, ICANON|ECHO {cooked:#x}\nterminal: {}",
                self.tail()
            );
        }

        fn resize(&self, rows: u16, cols: u16) {
            let size = libc::winsize {
                ws_row: rows,
                ws_col: cols,
                ws_xpixel: 0,
                ws_ypixel: 0,
            };
            let set = unsafe {
                libc::ioctl(
                    self.master.as_raw_fd(),
                    libc::TIOCSWINSZ as _,
                    &size as *const libc::winsize,
                )
            };
            assert_eq!(set, 0, "TIOCSWINSZ: {}", std::io::Error::last_os_error());
        }

        fn tail(&self) -> String {
            let output = self.output.lock().unwrap();
            String::from_utf8_lossy(&output[output.len().saturating_sub(2000)..]).into_owned()
        }
    }

    struct Scene {
        log: PathBuf,
        terminal: Terminal,
        run: i32,
    }

    impl Scene {
        fn events(&self) -> Vec<Value> {
            std::fs::read_to_string(&self.log)
                .unwrap_or_default()
                .lines()
                .filter_map(|line| serde_json::from_str(line).ok())
                .collect()
        }

        fn count(&self, key: &str) -> usize {
            self.events()
                .iter()
                .filter(|event| event.get(key).is_some())
                .count()
        }

        /// Waits until `done` holds for the stand-in's log.
        fn until(&self, what: &str, done: impl Fn(&[Value]) -> bool) {
            let deadline = Instant::now() + WAIT;
            while !done(&self.events()) {
                assert!(
                    Instant::now() < deadline,
                    "{what}\nlog: {:?}\nterminal: {}",
                    self.events(),
                    self.terminal.tail()
                );
                std::thread::sleep(Duration::from_millis(50));
            }
        }

        fn until_count(&self, key: &str, count: usize) {
            self.until(key, |events| {
                events
                    .iter()
                    .filter(|event| event.get(key).is_some())
                    .count()
                    >= count
            });
        }

        fn until_screen(&self, target: &Target, what: &str, done: impl Fn(&[String]) -> bool) {
            let Target::Run(socket) = target else {
                unreachable!()
            };
            let deadline = Instant::now() + WAIT;
            loop {
                if cctg::term::screen(socket).is_some_and(|lines| done(&lines)) {
                    return;
                }
                assert!(
                    Instant::now() < deadline,
                    "{what}\nterminal: {}",
                    self.terminal.tail()
                );
                std::thread::sleep(Duration::from_millis(50));
            }
        }

        /// The state of `cctg run`: `Some(code)` once it exited, `None`
        /// while it runs; `stopped` when it is stopped now.
        fn status(&self) -> (Option<i32>, bool) {
            let mut status = 0;
            let got =
                unsafe { libc::waitpid(self.run, &mut status, libc::WNOHANG | libc::WUNTRACED) };
            if got == 0 {
                return (None, false);
            }
            assert_eq!(
                got,
                self.run,
                "waitpid: {}",
                std::io::Error::last_os_error()
            );
            if libc::WIFSTOPPED(status) {
                return (None, true);
            }
            (Some(libc::WEXITSTATUS(status)), false)
        }
    }

    fn box_is(lines: &[String], text: &str) -> bool {
        keys::input_box(lines).is_some_and(|found| {
            found.len() == 1 && found[0].trim_start_matches('\u{276f}').trim() == text
        })
    }

    #[allow(
        clippy::zombie_processes,
        reason = "cctg run is reaped with waitpid, which also reports its stop"
    )]
    pub fn scenario() {
        let root = std::env::temp_dir().join(format!("cctg-pty-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (state, home) = (root.join("state"), root.join("home"));
        std::fs::create_dir_all(&state).unwrap();
        std::fs::create_dir_all(&home).unwrap();
        let log = root.join("claude.log");
        let terminal = Terminal::open();
        let mut command = super::common::cctg(&home);
        command
            .args(["run", "--", "--first"])
            .env("CCTG_CLAUDE", std::env::current_exe().unwrap())
            .env("RUN_PTY_ROLE", "claude")
            .env("RUN_PTY_LOG", &log)
            .env("CCTG_STATE_DIR", &state)
            .stdin(Stdio::from(terminal.slave.try_clone().unwrap()))
            .stdout(Stdio::from(terminal.slave.try_clone().unwrap()))
            .stderr(Stdio::from(terminal.slave.try_clone().unwrap()));
        // The terminal is `cctg run`'s controlling terminal, as a shell's is.
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() == -1 || libc::ioctl(0, libc::TIOCSCTTY as _, 0) == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let child = command.spawn().expect("cctg run starts");
        let scene = Scene {
            log: log.clone(),
            terminal,
            run: child.id() as i32,
        };
        let socket = cctg::term::socket_path(&state, child.id());
        let target = Target::Run(socket.clone());

        // The channels dialog is answered by `cctg run` itself.
        scene.until_count("dialog", 1);
        let events = scene.events();
        let first = &events[0];
        assert_eq!(first["start"], json!(["--first"]));
        assert_eq!(first["run"], child.id().to_string(), "CCTG_RUN");
        assert_eq!(
            first["size"],
            json!([30, 100]),
            "claude gets the user's size"
        );
        scene
            .terminal
            .expect_raw(true, "the user's terminal is in raw mode");
        scene.until_screen(&target, "the box", |lines| box_is(lines, ""));
        let mode = std::fs::metadata(socket.parent().unwrap()).unwrap();
        assert_eq!(
            std::os::unix::fs::PermissionsExt::mode(&mode.permissions()) & 0o777,
            0o700
        );

        // A panel command: typed, read, closed with Esc.
        let (typed, panel) = keys::type_command(&target, "/cost");
        assert_eq!(typed, Typed::Sent);
        assert!(panel.is_some_and(|text| text.contains("Total cost: $0")));
        scene.until_count("esc", 1);

        // A draft of the user's: nothing is sent, the draft stays.
        scene.terminal.type_keys(b"wip");
        scene.until_screen(&target, "the draft", |lines| box_is(lines, "wip"));
        assert_eq!(keys::type_line(&target, "/model x"), Typed::Draft);
        scene.until_screen(&target, "the draft stays", |lines| box_is(lines, "wip"));
        scene.terminal.type_keys(b"\x7f\x7f\x7f");
        scene.until_screen(&target, "the box again", |lines| box_is(lines, ""));
        assert!(
            !scene
                .events()
                .iter()
                .any(|event| event["submit"] == json!("/model x")),
            "nothing sent over a draft"
        );

        // Esc from the topic (⏹), Ctrl+C from the keyboard: bytes for claude.
        assert!(keys::press(&target, ConsoleKey::Interrupt));
        scene.until_count("esc", 2);
        scene.terminal.type_keys(b"\x03");
        scene.until_count("ctrl_c", 1);
        assert_eq!(
            scene.status(),
            (None, false),
            "Ctrl+C does not end cctg run"
        );

        // The window size follows the user's terminal.
        scene.terminal.resize(40, 120);
        std::thread::sleep(Duration::from_millis(300));
        scene.terminal.type_keys(b"z\x7f");
        scene.until("the new size", |events| {
            events.iter().any(|event| event["size"] == json!([40, 120]))
        });
        scene.until_screen(&target, "a copy of 40 rows", |lines| lines.len() == 40);

        // TASK-047: the agent view blocks typing; `/exit` over background
        // work opens a dialog, which is cancelled.
        assert_eq!(keys::type_line(&target, "/view"), Typed::Sent);
        scene.until_screen(&target, "the agent view", keys::agents_block);
        assert!(keys::agents_on_screen(&target));
        assert_eq!(keys::type_line(&target, "/x"), Typed::Agents);
        assert!(keys::press(&target, ConsoleKey::Interrupt));
        scene.until_screen(&target, "the view closed", |lines| box_is(lines, ""));
        assert_eq!(keys::type_line(&target, "/busy"), Typed::Sent);
        scene.until_screen(&target, "busy", |lines| box_is(lines, ""));
        assert_eq!(keys::type_exit(&target), Typed::Agents);
        assert_eq!(scene.count("exit"), 0, "claude stays");
        assert_eq!(keys::type_line(&target, "/idle"), Typed::Sent);

        // Ctrl+Z: claude stops itself, `cctg run` stops with it and gives the
        // terminal back in its own mode; `fg` (SIGCONT) resumes both.
        scene.until_screen(&target, "idle", |lines| box_is(lines, ""));
        scene.terminal.type_keys(b"\x1a");
        scene.until_count("suspend", 1);
        let deadline = Instant::now() + WAIT;
        while scene.status() != (None, true) {
            assert!(Instant::now() < deadline, "cctg run stops with claude");
            std::thread::sleep(Duration::from_millis(50));
        }
        scene
            .terminal
            .expect_raw(false, "the user's mode while stopped");
        // claude's last line went out before `cctg run` stopped: nothing
        // can write it while it is stopped.
        let deadline = Instant::now() + WAIT;
        while !scene.terminal.tail().contains(SUSPENDED) {
            assert!(
                Instant::now() < deadline,
                "claude's last line before the stop: {}",
                scene.terminal.tail()
            );
            std::thread::sleep(Duration::from_millis(50));
        }
        unsafe { libc::kill(scene.run, libc::SIGCONT) };
        scene.until_count("resumed", 1);
        scene.terminal.expect_raw(true, "raw again after fg");

        // "Обновить": the request and `/exit`; `cctg run` starts claude
        // again with `--resume` and answers its dialog again.
        let worker = Worker {
            claude_pid: Some(1),
            run_pid: Some(child.id()),
            run_args: vec!["--first".to_owned()],
            state_dir: Some(state.clone()),
            console: Some(target.clone()),
            ..Worker::default()
        };
        assert!(worker.restartable());
        assert_eq!(worker.restart(SESSION), Typed::Sent);
        scene.until_count("dialog", 2);
        let second = scene
            .events()
            .into_iter()
            .filter(|event| event.get("start").is_some())
            .nth(1)
            .unwrap();
        assert_eq!(second["start"], json!(["--first", "--resume", SESSION]));
        assert_eq!(second["size"], json!([40, 120]));
        scene.until_screen(&target, "the second box", |lines| box_is(lines, ""));

        // The second claude exits with 7: so does `cctg run`, the terminal
        // back in its own mode, the socket gone.
        assert_eq!(keys::type_exit(&target), Typed::Sent);
        let deadline = Instant::now() + WAIT;
        let code = loop {
            if let (Some(code), _) = scene.status() {
                break code;
            }
            assert!(Instant::now() < deadline, "cctg run ends");
            std::thread::sleep(Duration::from_millis(50));
        };
        assert_eq!(code, 7, "claude's own exit code");
        scene
            .terminal
            .expect_raw(false, "the terminal mode is restored");
        assert!(!socket.exists(), "the socket is removed");
        assert!(
            std::fs::read_dir(state.join("restart"))
                .unwrap()
                .next()
                .is_none(),
            "the request was taken"
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}

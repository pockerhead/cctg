# Reviewer-2 fixes on top of the planner patch (TASK-044).
# Usage: python fixes.py <workspace root>   (files are written with LF)
import sys

root = sys.argv[1]


def edit(path, pairs):
    full = f"{root}/{path}"
    s = open(full, encoding="utf-8").read().replace("\r\n", "\n")
    for old, new in pairs:
        assert s.count(old) == 1, (path, old[:60], s.count(old))
        s = s.replace(old, new)
    open(full, "w", encoding="utf-8", newline="\n").write(s)


edit("crates/cctg/src/term.rs", [
    (
        "                    &mut modes,\n                    &mut size,\n",
        "                    &raw mut modes,\n                    &raw mut size,\n",
    ),
    (
        "            let pid = pid as libc::pid_t;\n"
        "            self.claude.store(pid, Ordering::SeqCst);\n"
        "            let code = loop {",
        "            let pid = pid as libc::pid_t;\n"
        "            self.claude.store(pid, Ordering::SeqCst);\n"
        "            if self.stopping() {\n"
        "                // SIGTERM or SIGHUP came while this claude was being\n"
        "                // started: [`Host::hang_up`] found no claude to tell.\n"
        "                // SAFETY: a signal to claude's own process group.\n"
        "                unsafe {\n"
        "                    libc::kill(-pid, libc::SIGHUP);\n"
        "                }\n"
        "            }\n"
        "            let code = loop {",
    ),
    (
        "                if libc::WIFSTOPPED(status) {\n"
        "                    self.restore();",
        "                if libc::WIFSTOPPED(status) {\n"
        "                    // What claude wrote before it stopped (its terminal\n"
        "                    // modes undone, the \"suspended\" line) reaches the\n"
        "                    // terminal before the shell takes it back.\n"
        "                    std::thread::sleep(std::time::Duration::from_millis(100));\n"
        "                    self.restore();",
    ),
    (
        "                for stream in listener.incoming().flatten() {\n",
        "                for stream in listener.incoming() {\n"
        "                    // A failing accept (no descriptors left) is retried\n"
        "                    // later, not in a busy loop.\n"
        "                    let Ok(stream) = stream else {\n"
        "                        std::thread::sleep(std::time::Duration::from_millis(100));\n"
        "                        continue;\n"
        "                    };\n",
    ),
])

edit("crates/cctg/tests/run_pty_e2e.rs", [
    (
        "                    std::ptr::null_mut::<libc::termios>(),\n"
        "                    &mut size,",
        "                    std::ptr::null_mut::<libc::termios>(),\n"
        "                    &raw mut size,",
    ),
    (
        "let mut command = common::cctg(&home);",
        "let mut command = super::common::cctg(&home);",
    ),
    (
        "    const WAIT: Duration = Duration::from_secs(20);\n",
        "    const WAIT: Duration = Duration::from_secs(20);\n"
        "    const SUSPENDED: &str = \"fake claude suspended; fg to resume\";\n",
    ),
    (
        "                        log(json!({ \"suspend\": true }));\n",
        "                        log(json!({ \"suspend\": true }));\n"
        "                        // Like Claude Code: a last line, then SIGSTOP.\n"
        "                        print!(\"{SUSPENDED}\\r\\n\");\n"
        "                        std::io::stdout().flush().unwrap();\n",
    ),
    (
        "        let Target::Run(socket_path) = &target else {\n"
        "            unreachable!()\n"
        "        };\n"
        "        assert_eq!(\n"
        "            cctg::term::screen(socket_path).map(|lines| lines.len()),\n"
        "            Some(40)\n"
        "        );\n",
        "        scene.until_screen(&target, \"a copy of 40 rows\", |lines| lines.len() == 40);\n",
    ),
    (
        "        assert!(!scene.terminal.raw(), \"the user's mode while stopped\");\n",
        "        assert!(!scene.terminal.raw(), \"the user's mode while stopped\");\n"
        "        // claude's last line went out before `cctg run` stopped: nothing\n"
        "        // can write it while it is stopped.\n"
        "        let deadline = Instant::now() + WAIT;\n"
        "        while !scene.terminal.tail().contains(SUSPENDED) {\n"
        "            assert!(\n"
        "                Instant::now() < deadline,\n"
        "                \"claude's last line before the stop: {}\",\n"
        "                scene.terminal.tail()\n"
        "            );\n"
        "            std::thread::sleep(Duration::from_millis(50));\n"
        "        }\n",
    ),
])
print("ok")

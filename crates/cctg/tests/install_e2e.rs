//! `install.sh` end to end (TASK-031) on the system the tests run on: `sh`
//! on Linux and macOS, Git Bash on Windows. The script goes in on stdin
//! (`sh -s -- ...`), as with `curl ... | sh`.
//!
//! Nothing reaches the network: the release (this build's `cctg` under the
//! asset name of this system, plus `SHA256SUMS`) and the raw deploy files
//! come from a local HTTP server; Anthropic's installer, `docker` and
//! `cargo` are stand-ins on `PATH`. Every run has its own home directory;
//! the developer's `~/.cctg`, `~/.claude` and `~/.claude.json` are never
//! used (`common::isolate`), and every `cctg doctor` talks to a hub of the
//! test. The home folder has a non-ASCII name (a Cyrillic Windows user).
//! On Unix the shell runs in its own session: no terminal, so no question
//! can reach the developer's screen.

use std::collections::BTreeSet;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::Value;

mod common;

/// Every character dotenv or sh could take for syntax (visible ASCII only:
/// the hub takes nothing else).
const SECRET: &str = r#"in'st$HOME#"x\y`z-0123456789"#;
const TOKEN: &str = "123456:install-e2e-token-value";
const EXE: &str = std::env::consts::EXE_SUFFIX;
/// The line install.sh adds to the shell's start file (TASK-052).
const PATH_LINE: &str = "export PATH=\"$HOME/.local/bin:$PATH\" # cctg";
const EVENTS: [&str; 11] = [
    "PermissionRequest",
    "PostToolUse",
    "PostToolUseFailure",
    "PreCompact",
    "PreToolUse",
    "SessionEnd",
    "SessionStart",
    "Stop",
    "SubagentStart",
    "SubagentStop",
    "UserPromptSubmit",
];

fn install_sh() -> Vec<u8> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../install.sh");
    std::fs::read(path).expect("install.sh at the repository root")
}

/// The release tag the script belongs to (its `RELEASE=` line).
fn release_tag() -> String {
    let script = String::from_utf8(install_sh()).unwrap();
    script
        .lines()
        .find_map(|line| line.strip_prefix("RELEASE="))
        .expect("RELEASE= in install.sh")
        .to_owned()
}

/// This system's asset of the script's release, as release.yml names it.
fn asset() -> String {
    let triple = if cfg!(windows) {
        "x86_64-pc-windows-msvc.exe"
    } else if cfg!(target_os = "macos") {
        "aarch64-apple-darwin"
    } else {
        "x86_64-unknown-linux-musl"
    };
    format!("cctg-{}-{triple}", release_tag())
}

struct Root(PathBuf);

impl Root {
    fn new(name: &str) -> Self {
        let path =
            std::env::temp_dir().join(format!("cctg-install-e2e-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }

    fn dir(&self, name: &str) -> PathBuf {
        let dir = self.0.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }
}

impl Drop for Root {
    fn drop(&mut self) {
        for _ in 0..50 {
            if std::fs::remove_dir_all(&self.0).is_ok() || !self.0.exists() {
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

struct Killed(Child);

impl Drop for Killed {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

// ------------------------------------------------------------ fake release

/// Serves the files under `dir` over HTTP/1.1 on loopback; `GET /a/b` is
/// `dir/a/b`. The requested paths are kept.
fn serve(dir: PathBuf) -> (String, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
    let asked = Arc::new(Mutex::new(Vec::new()));
    let log = asked.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut request = String::new();
            if reader.read_line(&mut request).is_err() {
                continue;
            }
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                    break;
                }
            }
            let path = request.split(' ').nth(1).unwrap_or("/").to_owned();
            log.lock().unwrap().push(path.clone());
            let file = dir.join(path.trim_start_matches('/'));
            let answer = match std::fs::read(&file) {
                Ok(body) if file.is_file() => [
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    )
                    .into_bytes(),
                    body,
                ]
                .concat(),
                _ => b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                    .to_vec(),
            };
            let _ = stream.write_all(&answer);
        }
    });
    (base, asked)
}

/// A release directory with `binary` as this system's asset.
fn release(dir: &Path, binary: &[u8]) {
    let file = dir.join(asset());
    common::write_program(&file, binary);
    let hash = cctg::client::build_of(&file).unwrap();
    std::fs::write(
        dir.join("SHA256SUMS"),
        format!("{hash}  {}\n0000  other-asset\n", asset()),
    )
    .unwrap();
}

fn this_build() -> Vec<u8> {
    std::fs::read(env!("CARGO_BIN_EXE_cctg")).unwrap()
}

// ------------------------------------------------------------ the shell

/// `PATH` without any Claude Code or claude-cctg of the developer, with
/// `fake` first.
fn path_with(fake: &Path) -> std::ffi::OsString {
    let current = std::env::var_os("PATH").unwrap_or_default();
    let kept = std::env::split_paths(&current).filter(|dir| {
        ["claude", "claude.exe", "claude-cctg", "claude-cctg.cmd"]
            .iter()
            .all(|name| !dir.join(name).exists())
    });
    std::env::join_paths(std::iter::once(fake.to_owned()).chain(kept)).unwrap()
}

#[cfg(windows)]
fn shell() -> Command {
    let bash = cctg::statusline::git_bash(&|name| std::env::var(name).ok(), &|path| path.is_file())
        .expect("Git Bash (Git for Windows) runs install.sh on Windows");
    Command::new(bash)
}

#[cfg(unix)]
fn shell() -> Command {
    use std::os::unix::process::CommandExt;
    let mut command = Command::new("sh");
    // A new session has no controlling terminal: /dev/tty cannot be opened,
    // as with `curl | sh` in CI.
    unsafe {
        command.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    command
}

struct Run {
    home: PathBuf,
    fake: PathBuf,
    work: PathBuf,
    env: Vec<(String, String)>,
    /// Variables of this process the script does not get.
    removed: Vec<String>,
}

impl Run {
    fn new(root: &Root) -> Self {
        Self {
            home: root.dir("home-Иван"),
            fake: root.dir("fake-bin"),
            work: root.dir("work"),
            env: Vec::new(),
            removed: Vec::new(),
        }
    }

    fn env(&mut self, name: &str, value: &str) -> &mut Self {
        self.env.retain(|(key, _)| key != name);
        self.env.push((name.to_owned(), value.to_owned()));
        self
    }

    fn env_remove(&mut self, name: &str) -> &mut Self {
        self.removed.push(name.to_owned());
        self
    }

    /// A stand-in program in the fake `PATH` directory (a shell script).
    fn fake_program(&self, name: &str, script: &str) {
        common::write_program(&self.fake.join(name), script.as_bytes());
    }

    fn install(&self, args: &[&str]) -> (Output, String) {
        let mut command = shell();
        common::isolate(&mut command, &self.home);
        command
            .arg("-s")
            .arg("--")
            .args(args)
            .env("PATH", path_with(&self.fake))
            .current_dir(&self.work)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for name in &self.removed {
            command.env_remove(name);
        }
        for (name, value) in &self.env {
            command.env(name, value);
        }
        let mut child = common::spawn(&mut command).expect("start the shell");
        child
            .stdin
            .take()
            .unwrap()
            .write_all(&install_sh())
            .unwrap();
        let output = child.wait_with_output().unwrap();
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        (output, text)
    }

    fn cctg_dir(&self) -> PathBuf {
        self.home.join(".cctg")
    }

    fn exe(&self) -> PathBuf {
        self.cctg_dir().join("bin").join(format!("cctg{EXE}"))
    }

    fn wrapper(&self) -> PathBuf {
        self.home.join(".local").join("bin").join("claude-cctg")
    }
}

/// A fake Claude Code that writes its arguments, one per line, to
/// `<home>/claude-args`.
const FAKE_CLAUDE: &str =
    "#!/bin/sh\nfor a in \"$@\"; do printf '%s\\n' \"$a\"; done > \"$HOME/claude-args\"\n";

/// The hub side `cctg doctor` talks to: the real hook endpoint with
/// [`SECRET`], and an agent listener that only accepts.
fn fake_hub(runtime: &tokio::runtime::Runtime) -> (String, String) {
    runtime.block_on(async {
        let hooks = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let hook_addr = hooks.local_addr().unwrap().to_string();
        let (tx, mut rx) = tokio::sync::mpsc::channel(8);
        tokio::spawn(cctg::hub::ingress::serve_hooks(
            hooks,
            cctg::wire::Secret::parse(SECRET).unwrap(),
            tx,
        ));
        tokio::spawn(async move { while rx.recv().await.is_some() {} });
        let agents = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let agent_addr = agents.local_addr().unwrap().to_string();
        tokio::spawn(async move {
            let mut kept = Vec::new();
            while let Ok((stream, _)) = agents.accept().await {
                kept.push(stream);
            }
        });
        (agent_addr, hook_addr)
    })
}

fn same_file(a: &Path, b: &Path) -> bool {
    std::fs::canonicalize(a).ok() == std::fs::canonicalize(b).ok()
}

fn modified(path: &Path) -> std::time::SystemTime {
    std::fs::metadata(path).unwrap().modified().unwrap()
}

// ------------------------------------------------------------ the tests

#[test]
fn install_update_and_uninstall_a_device() {
    let root = Root::new("device");
    let dist = root.dir("dist");
    release(&dist, &this_build());
    let (base, asked) = serve(dist.clone());
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let (agent_addr, hook_addr) = fake_hub(&runtime);

    let mut run = Run::new(&root);
    run.env("CCTG_INSTALL_BASE_URL", &base);
    run.fake_program("claude", FAKE_CLAUDE);
    // What must stay as it was: the user's Claude Code config, a file of
    // theirs in ~/.cctg, and a hand-written wrapper (moved aside once).
    let claude_dir = run.home.join(".claude");
    std::fs::create_dir_all(&claude_dir).unwrap();
    std::fs::write(claude_dir.join("settings.json"), "{\"mine\":1}\n").unwrap();
    std::fs::write(run.home.join(".claude.json"), "{\"mine\":2}\n").unwrap();
    std::fs::create_dir_all(run.cctg_dir()).unwrap();
    std::fs::write(run.cctg_dir().join("keep.txt"), "mine").unwrap();
    std::fs::create_dir_all(run.wrapper().parent().unwrap()).unwrap();
    std::fs::write(run.wrapper(), "#!/bin/sh\necho handmade\n").unwrap();
    let claude_json = run.home.join(".claude.json");
    // The user's shell start file: the PATH line joins it once and leaves
    // it as it was (Git Bash: ~/.bashrc whatever the shell).
    run.env("SHELL", "/bin/zsh");
    let rc = run
        .home
        .join(if cfg!(windows) { ".bashrc" } else { ".zshrc" });
    let rc_mine = "# mine\nalias ll='ls -l'\n";
    std::fs::write(&rc, rc_mine).unwrap();
    let untouched = || {
        assert_eq!(
            std::fs::read_to_string(claude_dir.join("settings.json")).unwrap(),
            "{\"mine\":1}\n"
        );
        assert_eq!(
            std::fs::read_to_string(&claude_json).unwrap(),
            "{\"mine\":2}\n"
        );
    };

    // 1. A fresh install, the secret from the environment.
    run.env("CCTG_HUB_SECRET", SECRET);
    let (output, text) = run.install(&[
        "--yes",
        "--agent-addr",
        &agent_addr,
        "--hook-addr",
        &hook_addr,
    ]);
    assert!(output.status.success(), "{text}");
    assert!(!text.contains(SECRET), "the secret was printed: {text}");
    assert!(text.contains("took the secret"), "cctg doctor: {text}");
    let asked_paths = asked.lock().unwrap().clone();
    assert!(
        asked_paths.contains(&format!("/{}", asset()))
            && asked_paths.contains(&"/SHA256SUMS".to_owned()),
        "{asked_paths:?}"
    );
    assert_eq!(std::fs::read(run.exe()).unwrap(), this_build());
    untouched();
    assert_eq!(
        std::fs::read_to_string(&rc).unwrap(),
        format!("{rc_mine}{PATH_LINE}\n"),
        "{text}"
    );
    assert!(text.contains("to PATH in "), "{text}");

    let conf = run.cctg_dir().join("claude");
    let mcp: Value =
        serde_json::from_slice(&std::fs::read(conf.join("mcp.json")).unwrap()).unwrap();
    let server = &mcp["mcpServers"]["cctg"];
    assert!(same_file(
        Path::new(server["command"].as_str().unwrap()),
        &run.exe()
    ));
    assert_eq!(server["args"], serde_json::json!(["agent"]));
    let settings: Value =
        serde_json::from_slice(&std::fs::read(conf.join("settings.json")).unwrap()).unwrap();
    let events: BTreeSet<&str> = settings["hooks"]
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(events, EVENTS.into_iter().collect());
    // The session's status line is cctg's (TASK-056): its own two lines, or
    // the user's command chained through it.
    assert_eq!(settings["statusLine"]["type"], "command");
    let mut commands = vec![
        settings["statusLine"]["command"]
            .as_str()
            .unwrap()
            .to_owned(),
    ];
    for groups in settings["hooks"].as_object().unwrap().values() {
        for group in groups.as_array().unwrap() {
            for hook in group["hooks"].as_array().unwrap() {
                commands.push(hook["command"].as_str().unwrap().to_owned());
            }
        }
    }
    for command in &commands {
        let (quoted, rest) = command[1..].split_once('"').unwrap();
        assert!(same_file(Path::new(quoted), &run.exe()), "{command}");
        assert!(
            rest.starts_with(" hook ") || rest == " statusline",
            "{command}"
        );
    }
    // The question hook group (TASK-038) is the one docs/hook-settings.json
    // shows, but for the command's path.
    let documented: Value =
        serde_json::from_str(include_str!("../../../docs/hook-settings.json")).unwrap();
    let question_group = |settings: &Value| {
        settings["hooks"]["PreToolUse"]
            .as_array()
            .unwrap()
            .iter()
            .find(|group| group["matcher"] == "AskUserQuestion")
            .cloned()
            .expect("an AskUserQuestion group")
    };
    let mut installed = question_group(&settings);
    let documented = question_group(&documented);
    let hook = &installed["hooks"][0];
    assert!(
        hook["command"]
            .as_str()
            .unwrap()
            .ends_with("\" hook PreToolUse"),
        "{hook}"
    );
    assert_eq!(hook["timeout"], 330);
    assert!(
        hook["statusMessage"]
            .as_str()
            .is_some_and(|text| !text.is_empty())
    );
    installed["hooks"][0]["command"] = documented["hooks"][0]["command"].clone();
    assert_eq!(installed, documented);
    // So is the compaction group (TASK-053), with its short timeout.
    let documented: Value =
        serde_json::from_str(include_str!("../../../docs/hook-settings.json")).unwrap();
    let documented_events: BTreeSet<&str> = documented["hooks"]
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(documented_events, events);
    let mut installed = settings["hooks"]["PreCompact"].clone();
    let hook = &installed[0]["hooks"][0];
    assert!(
        hook["command"]
            .as_str()
            .unwrap()
            .ends_with("\" hook PreCompact"),
        "{hook}"
    );
    assert_eq!(hook["timeout"], 5);
    installed[0]["hooks"][0]["command"] =
        documented["hooks"]["PreCompact"][0]["hooks"][0]["command"].clone();
    assert_eq!(installed, documented["hooks"]["PreCompact"]);

    let wrapper = std::fs::read_to_string(run.wrapper()).unwrap();
    assert!(
        wrapper.contains("cctg-install") && wrapper.contains(" run -- --mcp-config "),
        "{wrapper}"
    );
    assert!(wrapper.contains("server:cctg --settings "), "{wrapper}");
    let aside = run
        .wrapper()
        .with_file_name("claude-cctg.before-cctg-install");
    assert_eq!(
        std::fs::read_to_string(&aside).unwrap(),
        "#!/bin/sh\necho handmade\n"
    );
    if cfg!(windows) {
        let cmd = std::fs::read_to_string(run.wrapper().with_file_name("claude-cctg.cmd")).unwrap();
        assert!(
            cmd.starts_with("@echo off\r\n") && cmd.contains("%*\r\n"),
            "{cmd}"
        );
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(run.cctg_dir().join("device.env"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
        // The wrapper starts claude through cctg run, the prompt last.
        let mut shell = shell();
        common::isolate(&mut shell, &run.home);
        let status = common::status(
            shell
                .arg(run.wrapper())
                .arg("fix the bug")
                .env("PATH", path_with(&run.fake)),
        )
        .unwrap();
        assert!(status.success());
        let args = std::fs::read_to_string(run.home.join("claude-args")).unwrap();
        let args: Vec<&str> = args.lines().collect();
        assert_eq!(args[0], "--mcp-config");
        assert_eq!(
            &args[2..5],
            [
                "--dangerously-load-development-channels",
                "server:cctg",
                "--settings"
            ]
        );
        assert_eq!(args[6], "fix the bug");
    }

    // 2. Again with nothing new: nothing is rewritten, the secret stays.
    let files = [
        conf.join("mcp.json"),
        conf.join("settings.json"),
        run.cctg_dir().join("device.env"),
        run.wrapper(),
        run.exe(),
        rc.clone(),
    ];
    let before: Vec<_> = files.iter().map(|file| modified(file)).collect();
    std::thread::sleep(Duration::from_millis(1100));
    run.env.retain(|(key, _)| key != "CCTG_HUB_SECRET");
    let (output, text) = run.install(&["--yes"]);
    assert!(output.status.success(), "{text}");
    assert!(
        text.contains("binary unchanged") && text.contains("took the secret"),
        "{text}"
    );
    let after: Vec<_> = files.iter().map(|file| modified(file)).collect();
    assert_eq!(before, after, "an update without changes rewrote a file");

    // 3. A new release while a session runs from the installed binary: it
    // is replaced in place (on Windows the running file is renamed).
    // macOS: bytes after the signature are refused (codesign cannot sign
    // them, the kernel kills an unsigned arm64 binary); another ad hoc
    // signature with another identifier makes other valid bytes.
    let mut newer = this_build();
    if cfg!(target_os = "macos") {
        release(&dist, &newer);
        assert!(
            common::status(
                Command::new("codesign")
                    .args(["--force", "-s", "-", "-i", "cctg.install-e2e.newer"])
                    .arg(dist.join(asset()))
            )
            .unwrap()
            .success()
        );
    } else {
        newer.extend_from_slice(b"\0newer build\0");
        release(&dist, &newer);
    }
    let newer = std::fs::read(dist.join(asset())).unwrap();
    std::fs::write(
        dist.join("SHA256SUMS"),
        format!(
            "{}  {}\n",
            cctg::client::build_of(&dist.join(asset())).unwrap(),
            asset()
        ),
    )
    .unwrap();
    let mut session = Command::new(run.exe());
    common::isolate(&mut session, &run.home);
    let (program, args): (&str, &[&str]) = if cfg!(windows) {
        ("ping", &["-n", "30", "127.0.0.1"])
    } else {
        ("sleep", &["30"])
    };
    let session = Killed(
        common::spawn(
            session
                .arg("run")
                .arg("--")
                .args(args)
                .env("CCTG_CLAUDE", program)
                .stdout(Stdio::null())
                .stderr(Stdio::null()),
        )
        .unwrap(),
    );
    std::thread::sleep(Duration::from_millis(500));
    let (output, text) = run.install(&["--yes"]);
    assert!(output.status.success(), "{text}");
    assert_eq!(std::fs::read(run.exe()).unwrap(), newer);
    if cfg!(windows) {
        assert!(run.exe().with_file_name("cctg.old.exe").exists(), "{text}");
    }
    drop(session);
    untouched();

    // 4. Uninstall: only what the script wrote goes.
    let (output, text) = run.install(&["--uninstall"]);
    assert!(output.status.success(), "{text}");
    for gone in [
        run.exe(),
        run.exe().with_file_name("cctg.old.exe"),
        conf.join("mcp.json"),
        conf.join("settings.json"),
        run.cctg_dir().join("device.env"),
        run.wrapper().with_file_name("claude-cctg.cmd"),
        aside.clone(),
    ] {
        assert!(!gone.exists(), "{} left: {text}", gone.display());
    }
    // The hand-written wrapper is back in its place.
    assert_eq!(
        std::fs::read_to_string(run.wrapper()).unwrap(),
        "#!/bin/sh\necho handmade\n"
    );
    assert!(run.cctg_dir().join("keep.txt").exists());
    untouched();
    assert_eq!(std::fs::read_to_string(&rc).unwrap(), rc_mine, "{text}");
}

/// A hub that drops every connection: `cctg doctor` fails at once.
fn closed_hub() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    std::thread::spawn(move || for _ in listener.incoming() {});
    addr
}

#[test]
fn uninstall_keeps_what_the_script_did_not_write() {
    let root = Root::new("foreign");
    let dist = root.dir("dist");
    release(&dist, &this_build());
    let (base, _) = serve(dist);
    let hub = closed_hub();
    let mut run = Run::new(&root);
    run.env("CCTG_INSTALL_BASE_URL", &base)
        .env("CCTG_HUB_SECRET", SECRET);
    run.fake_program("claude", FAKE_CLAUDE);
    let mine = "# my notes\nCCTG_HOST=my-laptop\nCCTG_STATE_DIR=/somewhere\n";
    std::fs::create_dir_all(run.cctg_dir()).unwrap();
    std::fs::write(run.cctg_dir().join("device.env"), mine).unwrap();
    // The certificate pin as openssl prints it.
    let bytes: Vec<u8> = (0..32u8).map(|n| n.wrapping_mul(37) ^ 0xa5).collect();
    let colons: Vec<String> = bytes.iter().map(|b| format!("{b:02X}")).collect();
    let pin: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    let fingerprint = format!("sha256 Fingerprint={}", colons.join(":"));
    let (output, text) = run.install(&[
        "--yes",
        "--agent-addr",
        &hub,
        "--hook-addr",
        &hub,
        "--pin",
        &fingerprint,
    ]);
    assert!(output.status.success(), "{text}");
    let env = std::fs::read_to_string(run.cctg_dir().join("device.env")).unwrap();
    assert!(env.starts_with(mine), "{env}");
    assert!(
        env.contains(&format!("CCTG_HUB_CERT_SHA256='{pin}'\n")),
        "{env}"
    );

    // What cctg leaves next to the binary while it runs.
    let bin = run.cctg_dir().join("bin");
    let workers = bin.join("cctg-workers");
    std::fs::create_dir_all(&workers).unwrap();
    let leftovers = [
        workers.join(format!("cctg-1{EXE}")),
        bin.join("cctg.old.exe.1700000000"),
        bin.join(format!("cctg.install{EXE}")),
    ];
    for file in &leftovers {
        std::fs::write(file, b"x").unwrap();
    }
    let (output, text) = run.install(&["--uninstall"]);
    assert!(output.status.success(), "{text}");
    assert_eq!(
        std::fs::read_to_string(run.cctg_dir().join("device.env")).unwrap(),
        mine,
        "{text}"
    );
    assert!(!bin.exists(), "{text}");
    // Claude Code's installer shares ~/.local/bin.
    assert!(run.wrapper().parent().unwrap().is_dir());
}

/// In a container the host name is its id (TASK-052): with --yes the name
/// comes from CCTG_HOST, and --host replaces whatever is there.
#[test]
fn a_container_gets_its_host_name_written() {
    let root = Root::new("container");
    let dist = root.dir("dist");
    release(&dist, &this_build());
    let (base, _) = serve(dist);
    let hub = closed_hub();
    let mut run = Run::new(&root);
    run.env("CCTG_INSTALL_BASE_URL", &base)
        .env("CCTG_HUB_SECRET", SECRET)
        // What Podman and systemd-nspawn put in a container's environment.
        .env("container", "docker");
    run.fake_program("claude", FAKE_CLAUDE);
    let args = ["--yes", "--agent-addr", &hub, "--hook-addr", &hub];
    let with_host = [&args[..], &["--host", "box-2"]].concat();
    let device_env = run.cctg_dir().join("device.env");
    let env = || std::fs::read_to_string(&device_env).unwrap();
    let hosts = |env: &str| -> Vec<String> {
        env.lines()
            .filter(|line| line.contains("CCTG_HOST"))
            .map(str::to_owned)
            .collect()
    };

    if cfg!(target_os = "macos") {
        // A Mac is never a container: the sign is ignored and the Mac's
        // own name is written, as without it.
        let (output, text) = run.install(&args);
        assert!(output.status.success(), "{text}");
        assert!(!text.contains("warning: in a container"), "{text}");
        let hosts = hosts(&env());
        assert_eq!(hosts.len(), 2, "{hosts:?}");
        assert_eq!(
            hosts[0],
            "# the CCTG_HOST line below: written by install.sh (macOS gives cctg no host name)"
        );
    } else {
        // No name anywhere: a warning, nothing written.
        let (output, text) = run.install(&args);
        assert!(output.status.success(), "{text}");
        assert!(text.contains("warning: in a container"), "{text}");
        assert!(hosts(&env()).is_empty(), "{}", env());

        run.env("CCTG_HOST", "dev-box");
        let (output, text) = run.install(&args);
        assert!(output.status.success(), "{text}");
        assert!(!text.contains("warning: in a container"), "{text}");
        assert_eq!(
            hosts(&env()),
            [
                "# the CCTG_HOST line below: written by install.sh (in a container the host name is its id)",
                "CCTG_HOST=dev-box"
            ]
        );
        // Once written it stays.
        run.env("CCTG_HOST", "other-name");
        let (output, text) = run.install(&args);
        assert!(output.status.success(), "{text}");
        assert!(env().contains("CCTG_HOST=dev-box\n"), "{}", env());
    }

    // --host replaces our line, and a line the user wrote as well.
    let mine = format!("{}CCTG_HOST=mine\n", env());
    std::fs::write(&device_env, mine).unwrap();
    let (output, text) = run.install(&with_host);
    assert!(output.status.success(), "{text}");
    assert_eq!(
        hosts(&env()),
        [
            "# the CCTG_HOST line below: written by install.sh (--host)",
            "CCTG_HOST=box-2"
        ]
    );
    let again = env();
    let (output, text) = run.install(&with_host);
    assert!(output.status.success(), "{text}");
    assert_eq!(env(), again, "the same --host changed the file");

    let (output, text) = run.install(&["--uninstall"]);
    assert!(output.status.success(), "{text}");
    assert!(!device_env.exists(), "{text}");
}

#[test]
fn missing_or_bad_settings_without_a_terminal_write_nothing() {
    let root = Root::new("refused");
    let mut run = Run::new(&root);
    // Nothing may be downloaded.
    run.env("CCTG_INSTALL_BASE_URL", "http://127.0.0.1:9/");
    let empty = root.0.join("empty-secret.txt");
    std::fs::write(&empty, "\nthe secret on line two\n").unwrap();
    let empty = empty.to_string_lossy().into_owned();
    let local = ["--agent-addr", "127.0.0.1:9", "--hook-addr", "127.0.0.1:9"];
    let bad_host = [&local[..], &["--host", "my box"]].concat();
    let bad_code = [&local[..], &["--join", "ABCD-EFGH;rm -rf"]].concat();
    let cases: [(&[&str], Option<&str>, &str); 6] = [
        (&local, None, "no hub secret"),
        (&bad_code, None, "a join code has letters"),
        (&bad_host, Some(SECRET), "the host name takes"),
        (&["--hub-host", "192.0.2.10"], Some(SECRET), "needs --pin"),
        (
            &["--hub-host", "192.0.2.10", "--pin", "0123abcd"],
            Some(SECRET),
            "not a sha256 fingerprint",
        ),
        (&["--secret-file", &empty], None, "is empty"),
    ];
    for (args, secret, error) in cases {
        run.env.retain(|(key, _)| key != "CCTG_HUB_SECRET");
        if let Some(secret) = secret {
            run.env("CCTG_HUB_SECRET", secret);
        }
        let args = [&["--yes"], args].concat();
        let (output, text) = run.install(&args);
        assert!(!output.status.success(), "{args:?}: {text}");
        assert!(text.contains(error), "{args:?}: {text}");
        assert!(!run.cctg_dir().exists(), "{args:?} wrote: {text}");
    }
}

/// A device trades a join code for its own secret (TASK-045): `--join`
/// writes it into device.env through `cctg join`, prints neither, and a
/// spent code changes nothing.
#[test]
fn a_device_joins_with_a_code() {
    let root = Root::new("join");
    let dist = root.dir("dist");
    release(&dist, &this_build());
    let (base, _) = serve(dist);
    let state = root.dir("hub-state");
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let devices = cctg::hub::devices::Devices::open(&state, None).unwrap();
    let (agent_addr, hook_addr) = runtime.block_on(async {
        let hooks = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let hook_addr = hooks.local_addr().unwrap().to_string();
        let (tx, mut rx) = tokio::sync::mpsc::channel(8);
        tokio::spawn(cctg::hub::ingress::serve_hooks(hooks, devices.clone(), tx));
        tokio::spawn(async move { while rx.recv().await.is_some() {} });
        let agents = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let agent_addr = agents.local_addr().unwrap().to_string();
        tokio::spawn(async move {
            let mut kept = Vec::new();
            while let Ok((stream, _)) = agents.accept().await {
                kept.push(stream);
            }
        });
        (agent_addr, hook_addr)
    });
    let code = cctg::hub::devices::mint_code(&state, std::time::SystemTime::now()).unwrap();
    let mut run = Run::new(&root);
    run.env("CCTG_INSTALL_BASE_URL", &base);
    run.fake_program("claude", FAKE_CLAUDE);
    let args = [
        "--yes",
        "--agent-addr",
        &agent_addr,
        "--hook-addr",
        &hook_addr,
        "--host",
        "joined-box",
    ];
    let (output, text) = run.install(&[&args[..], &["--join", &code]].concat());
    assert!(output.status.success(), "{text}");
    assert!(text.contains("this device is joined-box ("), "{text}");
    assert!(text.contains("took the secret"), "cctg doctor: {text}");
    let env_file = run.cctg_dir().join("device.env");
    let env = std::fs::read_to_string(&env_file).unwrap();
    let secret = env
        .lines()
        .find_map(|line| line.strip_prefix("CCTG_HUB_SECRET='"))
        .and_then(|value| value.strip_suffix('\''))
        .expect("the device's secret");
    assert!(secret.starts_with("cctgd_"), "{env}");
    assert!(!text.contains(secret) && !text.contains(&code), "{text}");
    assert_eq!(devices.list().0[0].name, "joined-box");

    // Again without a code: the device keeps its secret (install.sh puts
    // its own lines in its own order).
    let secret = secret.to_owned();
    let (output, text) = run.install(&args);
    assert!(output.status.success(), "{text}");
    let env = std::fs::read_to_string(&env_file).unwrap();
    assert!(
        env.contains(&format!(
            "CCTG_HUB_SECRET='{secret}'
"
        )),
        "{env}"
    );
    assert_eq!(env.matches("CCTG_HUB_SECRET").count(), 1, "{env}");
    // The spent code: refused, device.env as it was.
    run.env("CCTG_JOIN_CODE", &code);
    let (output, text) = run.install(&args);
    assert!(!output.status.success(), "{text}");
    assert!(text.contains("wrong, already used or expired"), "{text}");
    assert_eq!(std::fs::read_to_string(&env_file).unwrap(), env);
    // Uninstall takes the secret line out like the other lines it wrote.
    run.env.retain(|(key, _)| key != "CCTG_JOIN_CODE");
    let (output, text) = run.install(&["--uninstall"]);
    assert!(output.status.success(), "{text}");
    assert!(!env_file.exists(), "{text}");
    drop(runtime);
}

#[test]
fn a_bad_checksum_installs_nothing() {
    let root = Root::new("checksum");
    let dist = root.dir("dist");
    release(&dist, &this_build());
    std::fs::write(
        dist.join("SHA256SUMS"),
        format!("{}  {}\n", "0".repeat(64), asset()),
    )
    .unwrap();
    let (base, _) = serve(dist);
    let mut run = Run::new(&root);
    run.env("CCTG_INSTALL_BASE_URL", &base)
        .env("CCTG_HUB_SECRET", SECRET);
    run.fake_program("claude", FAKE_CLAUDE);
    let (output, text) = run.install(&["--yes"]);
    assert!(!output.status.success(), "{text}");
    assert!(text.contains("checksum mismatch"), "{text}");
    let bin = run.cctg_dir().join("bin");
    let left: Vec<_> = std::fs::read_dir(&bin)
        .map(|dir| dir.flatten().collect())
        .unwrap_or_default();
    assert!(left.is_empty(), "{left:?}");
    assert!(!run.cctg_dir().join("device.env").exists());
}

#[test]
fn a_missing_claude_code_comes_from_anthropics_installer_when_asked() {
    let root = Root::new("claude");
    let dist = root.dir("dist");
    release(&dist, &this_build());
    let (base, _) = serve(dist);
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let (agent_addr, hook_addr) = fake_hub(&runtime);
    let mut run = Run::new(&root);
    run.env("CCTG_INSTALL_BASE_URL", &base)
        .env("CCTG_HUB_SECRET", SECRET);
    let hub = ["--agent-addr", &agent_addr, "--hook-addr", &hook_addr];
    // The official installer is never downloaded: a stand-in answers its
    // command (curl https://claude.ai/install.sh | bash; on Windows
    // powershell ... irm https://claude.ai/install.ps1 | iex).
    let stub = "mkdir -p \"$HOME/.local/bin\" && printf '#!/bin/sh\\n' > \"$HOME/.local/bin/claude\" && chmod 755 \"$HOME/.local/bin/claude\" && : > \"$HOME/official-installer-ran\"";
    if cfg!(windows) {
        run.fake_program(
            "powershell",
            &format!("#!/bin/sh\ncase \"$*\" in *https://claude.ai/install.ps1*) HOME=$(cygpath -u \"$USERPROFILE\"); {stub};; *) exit 9;; esac\n"),
        );
    } else {
        let real = std::env::split_paths(&std::env::var_os("PATH").unwrap())
            .map(|dir| dir.join("curl"))
            .find(|curl| curl.is_file())
            .expect("curl");
        run.fake_program(
            "curl",
            &format!(
                "#!/bin/sh\nfor a in \"$@\"; do case $a in https://claude.ai/*) printf '%s\\n' '{}'; exit 0;; esac; done\nexec '{}' \"$@\"\n",
                stub.replace('\'', "'\\''"),
                real.display()
            ),
        );
    }
    // Without --yes and without a terminal: skipped, the install goes on.
    let (output, text) = run.install(&hub);
    assert!(output.status.success(), "{text}");
    assert!(text.contains("skipped Claude Code"), "{text}");
    assert!(!run.home.join("official-installer-ran").exists());
    // With --yes: Anthropic's installer (the stand-in) runs first.
    let (output, text) = run.install(&["--yes"]);
    assert!(output.status.success(), "{text}");
    assert!(text.contains("took the secret"), "{text}");
    assert!(text.contains("installing Claude Code"), "{text}");
    assert!(run.home.join("official-installer-ran").exists(), "{text}");
    assert!(!text.contains("still not found"), "{text}");
}

#[test]
fn from_source_builds_with_cargo_in_the_clone() {
    let root = Root::new("source");
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let (agent_addr, hook_addr) = fake_hub(&runtime);
    let mut run = Run::new(&root);
    let target = root.dir("target");
    std::fs::write(run.work.join("Cargo.toml"), "[workspace]\n").unwrap();
    run.fake_program(
        "cargo",
        &format!(
            "#!/bin/sh\n[ \"$1 $2 $3 $4 $5\" = 'build --release --locked -p cctg' ] || exit 9\nmkdir -p \"$CARGO_TARGET_DIR/release\" && cp \"$FAKE_CARGO_BUILT\" \"$CARGO_TARGET_DIR/release/cctg{EXE}\"\n"
        ),
    );
    run.fake_program("claude", FAKE_CLAUDE);
    run.env("CCTG_HUB_SECRET", SECRET)
        .env("CARGO_TARGET_DIR", &target.to_string_lossy())
        .env("FAKE_CARGO_BUILT", env!("CARGO_BIN_EXE_cctg"))
        // Nothing may be downloaded.
        .env("CCTG_INSTALL_BASE_URL", "http://127.0.0.1:9/");
    let (output, text) = run.install(&[
        "--yes",
        "--from-source",
        "--agent-addr",
        &agent_addr,
        "--hook-addr",
        &hook_addr,
    ]);
    assert!(output.status.success(), "{text}");
    assert!(text.contains("took the secret"), "{text}");
    assert_eq!(std::fs::read(run.exe()).unwrap(), this_build());
}

#[test]
fn a_hub_is_set_up_with_docker_compose() {
    let root = Root::new("hub");
    let raw = root.dir("raw");
    let deploy = raw.join("deploy");
    std::fs::create_dir_all(&deploy).unwrap();
    for file in ["compose.yml", "compose.host.yml"] {
        let from = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../deploy")
            .join(file);
        std::fs::copy(from, deploy.join(file)).unwrap();
    }
    let (raw_url, _) = serve(raw);
    let mut run = Run::new(&root);
    // docker: every call goes to docker.log; the hub's log says it started.
    run.fake_program(
        "docker",
        "#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"$DOCKER_LOG\"\ncase \"$*\" in *' logs '*) echo 'hub-1  | INFO hub started, polling bot=x';; *' exec -T hub cctg hub code'*) echo 'ABCD-EFGH-JKMN-PQRS';; esac\nexit 0\n",
    );
    let docker_log = root.0.join("docker.log");
    let hub_dir = run.home.join("hub");
    let dir = hub_dir.to_string_lossy().into_owned();
    run.env("CCTG_INSTALL_RAW_URL", &raw_url)
        .env("DOCKER_LOG", &docker_log.to_string_lossy())
        .env("CCTG_BOT_TOKEN", TOKEN);
    let args = [
        "--hub",
        "--yes",
        "--dir",
        &dir,
        "--chat-id",
        "-1001234567",
        "--users",
        "1001,1002",
        "--proxy",
        "http://127.0.0.1:10809",
        "--public-host",
        "hub.example.org",
    ];
    let (output, text) = run.install(&args);
    assert!(output.status.success(), "{text}");
    assert!(!text.contains(TOKEN), "the token was printed: {text}");

    let env = std::fs::read_to_string(hub_dir.join("hub.env")).unwrap();
    let value = |key: &str| {
        env.lines()
            .find_map(|line| line.strip_prefix(&format!("{key}=")))
            .unwrap_or_else(|| panic!("{key} in hub.env"))
            .to_owned()
    };
    assert_eq!(value("CCTG_BOT_TOKEN"), TOKEN);
    assert_eq!(value("CCTG_CHAT_ID"), "-1001234567");
    assert_eq!(value("CCTG_ALLOWED_USER_IDS"), "1001,1002");
    assert_eq!(value("HTTPS_PROXY"), "http://127.0.0.1:10809");
    // How devices reach the hub, for /join (TASK-046): the host ports.
    assert_eq!(value("CCTG_PUBLIC_AGENT_ADDR"), "hub.example.org:47291");
    assert_eq!(value("CCTG_PUBLIC_HOOK_ADDR"), "hub.example.org:47292");
    let secret = value("CCTG_HUB_SECRET");
    assert!(
        cctg::wire::Secret::parse(&secret).is_ok() && secret.len() == 48,
        "{secret}"
    );
    let compose_env = std::fs::read_to_string(hub_dir.join(".env")).unwrap();
    let compose_env: Vec<&str> = compose_env.lines().collect();
    assert!(
        compose_env.contains(&"COMPOSE_FILE=compose.yml:compose.host.yml"),
        "a proxy on the host's loopback needs the host network: {compose_env:?}"
    );
    // The image of the script's own release, not `latest`.
    let image_tag = format!("CCTG_IMAGE_TAG={}", release_tag().trim_start_matches('v'));
    assert!(compose_env.contains(&image_tag.as_str()), "{compose_env:?}");
    assert!(
        compose_env.contains(&"CCTG_PUBLIC_HOST=hub.example.org"),
        "{compose_env:?}"
    );
    assert_eq!(
        std::fs::read(hub_dir.join("compose.yml")).unwrap(),
        std::fs::read(deploy.join("compose.yml")).unwrap()
    );
    // The hub takes the certificate and key the script made.
    let (_, pin) = cctg::tls::Acceptor::from_files(
        &hub_dir.join("tls").join("cert.pem"),
        &hub_dir.join("tls").join("key.pem"),
    )
    .expect("the hub loads the generated certificate");
    let pin = pin.to_string().replace(':', "").to_lowercase();
    let line = text
        .lines()
        .find(|line| line.starts_with("curl -fsSL"))
        .expect("the client line");
    // A one-time join code of the hub, never the shared secret (TASK-045).
    assert_eq!(
        line,
        format!(
            "curl -fsSL https://raw.githubusercontent.com/pockerhead/cctg/{}/install.sh \
             | sh -s -- --hub-host hub.example.org --pin {pin} --join ABCD-EFGH-JKMN-PQRS",
            release_tag()
        )
    );
    assert!(
        !text.contains(&secret),
        "the shared secret was printed: {text}"
    );
    let calls = std::fs::read_to_string(&docker_log).unwrap();
    assert!(
        calls.contains("compose pull hub")
            && calls.contains("compose up -d --force-recreate hub")
            && calls.contains("compose exec -T hub cctg hub code"),
        "{calls}"
    );

    // A later release changes compose.yml; the user never edited it: the
    // script of that release updates it. The address is kept from before.
    run.env.retain(|(key, _)| key != "CCTG_BOT_TOKEN");
    let mut release_compose = std::fs::read(deploy.join("compose.yml")).unwrap();
    release_compose.extend_from_slice(b"# a later release\n");
    std::fs::write(deploy.join("compose.yml"), &release_compose).unwrap();
    let again = ["--hub", "--yes", "--dir", &dir];
    let (output, text) = run.install(&again);
    assert!(output.status.success(), "{text}");
    assert_eq!(
        std::fs::read(hub_dir.join("compose.yml")).unwrap(),
        release_compose,
        "{text}"
    );
    assert!(!hub_dir.join("compose.yml.new").exists(), "{text}");
    assert!(text.contains("--hub-host hub.example.org"), "{text}");

    // Again without the token: everything kept, the same pin and secret;
    // a compose.yml changed by hand (other ports) stays.
    let mine = std::fs::read_to_string(hub_dir.join("compose.yml"))
        .unwrap()
        .replace("\"47291:47291\"", "\"52191:47291\"");
    std::fs::write(hub_dir.join("compose.yml"), &mine).unwrap();
    let (output, text) = run.install(&again);
    assert!(output.status.success(), "{text}");
    assert_eq!(
        std::fs::read_to_string(hub_dir.join("hub.env")).unwrap(),
        env
    );
    assert!(text.contains(&format!("--pin {pin}")), "{text}");
    assert_eq!(
        std::fs::read_to_string(hub_dir.join("compose.yml")).unwrap(),
        mine
    );
    assert_eq!(
        std::fs::read(hub_dir.join("compose.yml.new")).unwrap(),
        release_compose
    );
    // On the host network the published ports do not apply.
    assert!(text.contains("--hub-host hub.example.org"), "{text}");
    // A proxy on another machine: the bridge network, and the client line
    // takes the host ports of compose.yml.
    let (output, text) =
        run.install(&[&again[..], &["--proxy", "http://proxy.example:3128"]].concat());
    assert!(output.status.success(), "{text}");
    assert!(
        text.contains(
            " --agent-addr hub.example.org:52191 --hook-addr hub.example.org:47292 --pin "
        ),
        "{text}"
    );
    // /join says the same: the public lines follow the host ports, once each.
    let env = std::fs::read_to_string(hub_dir.join("hub.env")).unwrap();
    for line in [
        "CCTG_PUBLIC_AGENT_ADDR=hub.example.org:52191",
        "CCTG_PUBLIC_HOOK_ADDR=hub.example.org:47292",
    ] {
        assert!(env.lines().any(|l| l == line), "{line} in {env}");
    }
    assert_eq!(env.matches("CCTG_PUBLIC_").count(), 2, "{env}");

    // Stop: compose down; the secrets and the key stay for the user.
    let (output, text) = run.install(&["--hub", "--uninstall", "--dir", &dir]);
    assert!(output.status.success(), "{text}");
    assert!(
        std::fs::read_to_string(&docker_log)
            .unwrap()
            .contains("compose down")
    );
    assert!(!hub_dir.join("compose.yml").exists());
    assert!(hub_dir.join("hub.env").exists() && hub_dir.join("tls").join("key.pem").exists());
}

#[test]
fn a_hub_that_does_not_start_or_has_no_address_is_an_error() {
    let root = Root::new("hub-fails");
    let raw = root.dir("raw");
    let deploy = raw.join("deploy");
    std::fs::create_dir_all(&deploy).unwrap();
    for file in ["compose.yml", "compose.host.yml"] {
        let from = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../deploy")
            .join(file);
        std::fs::copy(from, deploy.join(file)).unwrap();
    }
    let (raw_url, _) = serve(raw);
    let mut run = Run::new(&root);
    // The hub's own start check fails: its log ends with "Error: ...".
    run.fake_program(
        "docker",
        "#!/bin/sh\ncase \"$*\" in *' logs '*) echo 'hub-1  | Error: the bot cannot manage topics in this group';; esac\nexit 0\n",
    );
    let hub_dir = run.home.join("hub");
    let dir = hub_dir.to_string_lossy().into_owned();
    run.env("CCTG_INSTALL_RAW_URL", &raw_url)
        .env("CCTG_BOT_TOKEN", TOKEN);
    let args = [
        "--hub",
        "--yes",
        "--dir",
        &dir,
        "--chat-id",
        "-1001234567",
        "--users",
        "1001",
    ];
    // Without a terminal the devices' address must be given: nothing is
    // started or written, and no placeholder line is printed.
    let (output, text) = run.install(&args);
    assert!(!output.status.success(), "{text}");
    assert!(text.contains("use --public-host"), "{text}");
    assert!(!hub_dir.exists(), "{text}");

    let (output, text) = run.install(&[&args[..], &["--public-host", "hub.example.org"]].concat());
    assert!(!output.status.success(), "{text}");
    assert!(
        text.contains("Error: the bot cannot manage topics")
            && text.contains("the hub did not start"),
        "{text}"
    );
    assert!(!text.contains("curl -fsSL"), "{text}");
    // No proxy is an answer too: the question is not asked again.
    let env = std::fs::read_to_string(hub_dir.join("hub.env")).unwrap();
    assert!(
        env.lines()
            .any(|line| line == "# HTTPS_PROXY: none (install.sh --hub)")
            && !env.contains("HTTPS_PROXY="),
        "{env}"
    );
}

/// The named `NAME=` lines and functions of install.sh, for a script that
/// runs them alone.
fn script_parts(constants: &[&str], functions: &[&str]) -> String {
    let script = String::from_utf8(install_sh()).unwrap();
    let mut parts = String::new();
    for name in constants {
        let line = script
            .lines()
            .find(|line| line.starts_with(&format!("{name}=")))
            .unwrap_or_else(|| panic!("{name}= in install.sh"));
        parts.push_str(line);
        parts.push('\n');
    }
    for name in functions {
        let start = script
            .find(&format!("\n{name}() {{\n"))
            .unwrap_or_else(|| panic!("{name}() in install.sh"))
            + 1;
        let end = start + script[start..].find("\n}\n").expect("the function's end") + 3;
        parts.push_str(&script[start..end]);
    }
    parts
}

/// The local hub's listeners in hub.env (TASK-046 review): the lines the
/// script chose are marked and chosen again for another `--public-host`,
/// the user's own lines stay, and this machine's client line goes where
/// each listener takes loopback connections: `[::1]` for `[::]`, which
/// takes IPv6 only on Windows.
#[test]
fn local_hub_listeners_follow_the_public_host_and_keep_the_users_lines() {
    let root = Root::new("listeners");
    let hub_env = root.0.join("hub.env");
    let parts = script_parts(
        &["PROXY_NONE", "LISTEN_MARK", "AGENT_PORT", "HOOK_PORT"],
        &[
            "line_of",
            "value_of",
            "hub_line",
            "put",
            "write_hub_env",
            "user_listen",
            "listeners",
            "loopback_of",
            "hub_where",
            "local_where",
        ],
    );
    // As setup_local_hub writes hub.env, then the client line.
    let script = format!(
        "set -eu\n{parts}hub_env=$1\npublic_host=$2\ntoken= chat_id= users= new_secret= proxy=\n\
         listeners\n\
         write_hub_env '# test' \"CCTG_PUBLIC_AGENT_ADDR$listen_keys\" \"$listen_lines\"\n\
         local_where\n"
    );
    let path = hub_env.to_string_lossy().replace('\\', "/");
    let step = |public_host: &str| -> (String, String) {
        let output = common::output(
            shell()
                .arg("-c")
                .arg(&script)
                .arg("sh")
                .arg(&path)
                .arg(public_host),
        )
        .unwrap();
        let text = String::from_utf8_lossy(&output.stdout).into_owned();
        assert!(
            output.status.success(),
            "{text}{}",
            String::from_utf8_lossy(&output.stderr)
        );
        (text, std::fs::read_to_string(&hub_env).unwrap())
    };
    let listen = |env: &str| -> Vec<String> {
        env.lines()
            .filter(|line| line.starts_with("CCTG_") && line.contains("_LISTEN="))
            .map(str::to_owned)
            .collect()
    };
    let marks = |env: &str| env.matches("rewritten on each run").count();

    // Only this machine: no listener lines, the defaults on 127.0.0.1.
    let (line, env) = step("");
    assert_eq!(line, "--hub-host 127.0.0.1");
    assert!(listen(&env).is_empty(), "{env}");
    // A host name: every IPv4 address.
    let (line, env) = step("hub.example.org");
    assert_eq!(
        listen(&env),
        [
            "CCTG_AGENT_LISTEN=0.0.0.0:47291",
            "CCTG_HOOK_LISTEN=0.0.0.0:47292"
        ]
    );
    assert_eq!(marks(&env), 2, "{env}");
    assert_eq!(line, "--hub-host 127.0.0.1");
    // Another run for an [IPv6] host: the same lines chosen again.
    let (line, env) = step("[2001:db8::1]");
    assert_eq!(
        listen(&env),
        [
            "CCTG_AGENT_LISTEN=[::]:47291",
            "CCTG_HOOK_LISTEN=[::]:47292"
        ]
    );
    assert_eq!(marks(&env), 2, "{env}");
    assert_eq!(line, "--hub-host '[::1]'");
    let (line, env) = step("[::1]");
    assert_eq!(
        listen(&env),
        [
            "CCTG_AGENT_LISTEN=[::1]:47291",
            "CCTG_HOOK_LISTEN=[::1]:47292"
        ]
    );
    assert_eq!(line, "--hub-host '[::1]'");

    // The user's own agent line stays for any host; the marked hook line
    // is chosen again.
    let users = env.replace(
        &format!(
            "{} CCTG_AGENT_LISTEN\nCCTG_AGENT_LISTEN=[::1]:47291\n",
            "# written by install.sh --hub --local, rewritten on each run:"
        ),
        "CCTG_AGENT_LISTEN=[::]:52191\n",
    );
    assert_ne!(users, env, "the marked agent line in {env}");
    std::fs::write(&hub_env, &users).unwrap();
    let (line, env) = step("hub.example.org");
    assert_eq!(
        listen(&env),
        [
            "CCTG_AGENT_LISTEN=[::]:52191",
            "CCTG_HOOK_LISTEN=0.0.0.0:47292"
        ]
    );
    assert_eq!(marks(&env), 1, "{env}");
    assert_eq!(
        line,
        "--agent-addr '[::1]:52191' --hook-addr 127.0.0.1:47292"
    );
    // Again: nothing changes.
    let (_, again) = step("hub.example.org");
    assert_eq!(again, env);
}

/// Windows: a Run value is a command line of at most 260 characters; a hub
/// folder that makes it longer is refused before anything is written.
#[test]
fn a_hub_folder_too_long_for_the_run_key_is_refused() {
    if !cfg!(windows) {
        return;
    }
    let root = Root::new("long-dir");
    let mut run = Run::new(&root);
    // Never the real Run key, whatever happens.
    run.env(
        "CCTG_INSTALL_RUN_KEY",
        &format!(
            "HKCU\\Software\\cctg-install-e2e-{}-long\\Run",
            std::process::id()
        ),
    );
    let hub_dir = root.0.join("h".repeat(200));
    let dir = hub_dir.to_string_lossy().into_owned();
    let (output, text) = run.install(&["--hub", "--local", "--yes", "--dir", &dir]);
    assert!(!output.status.success(), "{text}");
    assert!(
        text.contains("Windows runs at most 260 from the Run key"),
        "{text}"
    );
    assert!(!hub_dir.exists(), "{text}");
    assert!(!run.cctg_dir().exists(), "{text}");
}

#[test]
fn the_script_has_unix_line_endings() {
    // A CRLF checkout (core.autocrlf on Windows) breaks `sh`; .gitattributes
    // keeps it LF.
    assert!(!install_sh().contains(&b'\r'));
}

// ------------------------------------------------ the local hub (TASK-046)

const CHAT: i64 = -1000000000001;
const USER: i64 = 1001;
/// The local hub's proxy (only in hub.env; nothing listens there: the fake
/// Bot API is `http://`, which an HTTPS proxy leaves alone).
const PROXY: &str = "http://pu:pp-marker@127.0.0.1:9";
const PROXY_MARK: &str = "pp-marker";

/// A Bot API for a real hub: the start checks pass, updates come from the
/// test, sent messages are kept.
#[derive(Default)]
struct Bot {
    state: Mutex<BotState>,
    new_update: tokio::sync::Notify,
    /// The next this many getMe calls fail with a dropped connection.
    get_me_failures: std::sync::atomic::AtomicU32,
    /// After those, this many answer only after 3 s (Telegram far away).
    slow_get_me: std::sync::atomic::AtomicU32,
}

#[derive(Default)]
struct BotState {
    updates: Vec<Value>,
    /// `sendMessage` bodies with the message id the fake gave.
    sent: Vec<(i64, Value)>,
    /// `editMessageText` bodies.
    edits: Vec<Value>,
    messages: i64,
}

impl Bot {
    /// A message in General (no topic) from user `from`.
    fn push_general(&self, from: i64, text: &str) {
        let mut state = self.state.lock().unwrap();
        let id = state.updates.len() as i64 + 1;
        state.updates.push(serde_json::json!({
            "update_id": id,
            "message": {
                "message_id": 7000 + id,
                "date": 1,
                "text": text,
                "from": { "id": from, "is_bot": false, "first_name": "u" },
                "chat": { "id": CHAT, "type": "supergroup", "is_forum": true },
            },
        }));
        drop(state);
        self.new_update.notify_waiters();
    }

    fn sent(&self) -> Vec<(i64, Value)> {
        self.state.lock().unwrap().sent.clone()
    }

    fn edits(&self) -> Vec<Value> {
        self.state.lock().unwrap().edits.clone()
    }

    async fn answer(&self, method: &str, body: &Value) -> Value {
        let ok = |result: Value| serde_json::json!({"ok": true, "result": result});
        match method {
            "getMe" => ok(
                serde_json::json!({"id": 3003, "is_bot": true, "first_name": "b", "username": "fake_bot"}),
            ),
            "getChatMember" => ok(serde_json::json!({
                "status": "administrator", "can_manage_topics": true, "can_delete_messages": true,
            })),
            "getForumTopicIconStickers" => ok(Value::Array(
                [
                    cctg::hub::registry::ICON_ALIVE,
                    cctg::hub::registry::ICON_DEAD,
                    cctg::hub::registry::ICON_WAITING,
                    cctg::hub::registry::ICON_NO_CHANNEL,
                ]
                .iter()
                .map(|id| serde_json::json!({"custom_emoji_id": id, "emoji": "x"}))
                .collect(),
            )),
            "getUpdates" => ok(Value::Array(self.updates(body).await)),
            "sendMessage" | "editMessageText" => {
                let mut state = self.state.lock().unwrap();
                state.messages += 1;
                let id = state.messages;
                if method == "sendMessage" {
                    state.sent.push((id, body.clone()));
                } else {
                    state.edits.push(body.clone());
                }
                ok(serde_json::json!({
                    "message_id": state.messages,
                    "date": 1,
                    "chat": { "id": CHAT },
                }))
            }
            _ => ok(Value::Bool(true)),
        }
    }

    /// Updates at or past the offset, waiting up to a second for one.
    async fn updates(&self, body: &Value) -> Vec<Value> {
        let offset = body["offset"].as_i64().unwrap_or(0);
        let deadline = tokio::time::Instant::now() + Duration::from_secs(1);
        loop {
            let waiting = self.new_update.notified();
            let found: Vec<Value> = self
                .state
                .lock()
                .unwrap()
                .updates
                .iter()
                .filter(|update| update["update_id"].as_i64().unwrap_or(0) >= offset)
                .cloned()
                .collect();
            if !found.is_empty() {
                return found;
            }
            if tokio::time::timeout_at(deadline, waiting).await.is_err() {
                return Vec::new();
            }
        }
    }
}

async fn serve_bot(bot: Arc<Bot>) -> u16 {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            let bot = bot.clone();
            tokio::spawn(async move {
                // One request per connection, answered with Connection: close.
                let mut buf = Vec::new();
                let head_end = loop {
                    let mut chunk = [0u8; 4096];
                    let Ok(n) = stream.read(&mut chunk).await else {
                        return;
                    };
                    if n == 0 {
                        return;
                    }
                    buf.extend_from_slice(&chunk[..n]);
                    if let Some(at) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                        break at + 4;
                    }
                };
                let head = String::from_utf8_lossy(&buf[..head_end]).into_owned();
                let length = head
                    .lines()
                    .find_map(|line| {
                        let (name, value) = line.split_once(':')?;
                        name.eq_ignore_ascii_case("content-length")
                            .then(|| value.trim().parse::<usize>().ok())?
                    })
                    .unwrap_or(0);
                while buf.len() < head_end + length {
                    let mut chunk = [0u8; 4096];
                    match stream.read(&mut chunk).await {
                        Ok(0) | Err(_) => break,
                        Ok(n) => buf.extend_from_slice(&chunk[..n]),
                    }
                }
                let path = head.split_whitespace().nth(1).unwrap_or_default();
                let method = path.rsplit('/').next().unwrap_or_default().to_owned();
                let body: Value = serde_json::from_slice(&buf[head_end..]).unwrap_or(Value::Null);
                if method == "getMe"
                    && bot
                        .get_me_failures
                        .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
                        .is_ok()
                {
                    // A network error: the hub asks again (START_TRIES).
                    tokio::time::sleep(Duration::from_secs(3)).await;
                    return;
                }
                if method == "getMe"
                    && bot
                        .slow_get_me
                        .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
                        .is_ok()
                {
                    tokio::time::sleep(Duration::from_secs(3)).await;
                }
                let answer = bot.answer(&method, &body).await.to_string();
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{answer}",
                    answer.len()
                );
                let _ = stream.write_all(response.as_bytes()).await;
                let _ = stream.shutdown().await;
            });
        }
    });
    port
}

fn listening(port: u16) -> bool {
    std::net::TcpStream::connect_timeout(
        &std::net::SocketAddr::from(([127, 0, 0, 1], port)),
        Duration::from_secs(2),
    )
    .is_ok()
}

fn wait_until(what: &str, mut done: impl FnMut() -> bool) {
    for _ in 0..600 {
        if done() {
            return;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    panic!("timed out waiting for {what}");
}

/// A stand-in `systemctl` (Linux) that runs the unit's ExecStart line in the
/// background on restart and stops it with SIGTERM, as systemd would.
const FAKE_SYSTEMCTL: &str = r#"#!/bin/sh
printf '%s\n' "$*" >> "$FAKE_LOG"
unit=$XDG_CONFIG_HOME/systemd/user/cctg-hub.service
pidfile=$FAKE_DIR/hub.pid
stop() {
    [ -f "$pidfile" ] || return 0
    pid=$(cat "$pidfile")
    kill -TERM "$pid" 2>/dev/null
    i=0
    while kill -0 "$pid" 2>/dev/null && [ $i -lt 450 ]; do sleep 0.1; i=$((i + 1)); done
    rm -f "$pidfile"
}
case "$*" in
    "--user restart cctg-hub.service")
        stop
        cmd=$(sed -n 's/^ExecStart=//p' "$unit")
        eval "set -- $cmd"
        "$@" </dev/null >/dev/null 2>&1 &
        echo $! > "$pidfile"
        ;;
    "--user disable --now cctg-hub.service") stop ;;
esac
exit 0
"#;

/// A stand-in `launchctl` (macOS): bootstrap runs the plist's
/// ProgramArguments in the background, bootout stops them with SIGTERM.
const FAKE_LAUNCHCTL: &str = r#"#!/bin/sh
printf '%s\n' "$*" >> "$FAKE_LOG"
pidfile=$FAKE_DIR/hub.pid
case $1 in
    print)
        [ -f "$pidfile" ] && kill -0 "$(cat "$pidfile")" 2>/dev/null && exit 0
        exit 113
        ;;
    bootout)
        [ -f "$pidfile" ] || exit 3
        kill -TERM "$(cat "$pidfile")" 2>/dev/null
        rm -f "$pidfile"
        ;;
    bootstrap)
        args=$(sed -n '/<key>ProgramArguments<\/key>/,/<\/array>/s/.*<string>\(.*\)<\/string>.*/\1/p' "$3")
        IFS='
'
        # shellcheck disable=SC2086
        set -- $args
        "$@" </dev/null >/dev/null 2>&1 &
        echo $! > "$pidfile"
        ;;
esac
exit 0
"#;

/// Stops whatever the test started, also when a check failed: the fake
/// service's process and any supervisor of the bin folder (`cctg.stop`);
/// on Windows the test's registry key goes.
struct LocalHub {
    bin: PathBuf,
    fake_state: PathBuf,
    run_key: String,
    armed: bool,
}

impl Drop for LocalHub {
    fn drop(&mut self) {
        if self.armed {
            if let Ok(pid) = std::fs::read_to_string(self.fake_state.join("hub.pid")) {
                let _ = common::status(Command::new("kill").args(["-TERM", pid.trim()]));
            }
            let stop = self.bin.join("cctg.stop");
            if std::fs::write(&stop, "").is_ok() {
                for _ in 0..400 {
                    if !stop.exists() {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
                let _ = std::fs::remove_file(&stop);
            }
        }
        // Only the test's own key, whatever a failed check left in run_key.
        if cfg!(windows)
            && self
                .run_key
                .starts_with("HKCU\\Software\\cctg-install-e2e-")
        {
            let parent = self.run_key.trim_end_matches("\\Run");
            let _ = common::status(
                Command::new("reg")
                    .args(["delete", parent, "/f"])
                    .stdout(Stdio::null())
                    .stderr(Stdio::null()),
            );
        }
    }
}

/// The `cctg-hub` value of `key`, read from `reg export` (UTF-16: the
/// console output of `reg query` garbles a non-ASCII path).
#[cfg(windows)]
fn run_value(key: &str) -> Option<String> {
    let file = std::env::temp_dir().join(format!("cctg-install-e2e-{}.reg", std::process::id()));
    let exported = common::status(
        Command::new("reg")
            .args(["export", key])
            .arg(&file)
            .arg("/y")
            .stdout(Stdio::null())
            .stderr(Stdio::null()),
    )
    .is_ok_and(|status| status.success());
    let bytes = std::fs::read(&file).unwrap_or_default();
    let _ = std::fs::remove_file(&file);
    if !exported {
        return None;
    }
    let units: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
        .collect();
    let text = String::from_utf16_lossy(&units);
    let value = text
        .lines()
        .find_map(|line| line.strip_prefix("\"cctg-hub\"=\""))?
        .trim_end()
        .strip_suffix('"')?;
    Some(value.replace("\\\"", "\"").replace("\\\\", "\\"))
}

/// `--hub --local` (TASK-046) on the system the tests run on: the hub is
/// installed, registered to start at logon (a stand-in systemctl or
/// launchctl; on Windows the Run value under a test key, started by the real
/// Windows Script Host, hidden) and started; `/join` in General answers only
/// the allowlisted user, and its line installs another device against this
/// hub, after which the message says the code was used; a second run
/// restarts the hub on the binary; `--uninstall` stops it and removes the
/// autostart, hub.env and state stay. The hub folder has a space and
/// Cyrillic in its name; the proxy stays in hub.env only.
#[test]
fn a_local_hub_starts_at_logon_and_join_gives_a_working_line() {
    let root = Root::new("local-hub");
    let dist = root.dir("dist");
    release(&dist, &this_build());
    let (base, _) = serve(dist);
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let bot = Arc::new(Bot::default());
    let api = runtime.block_on(serve_bot(bot.clone()));

    let mut run = Run::new(&root);
    let (agent_port, hook_port) = (common::free_port(), common::free_port());
    let hub_dir = root.0.join("мой hub");
    std::fs::create_dir_all(&hub_dir).unwrap();
    let hub_dir_arg = hub_dir.to_string_lossy().into_owned();
    // The user's own lines: the fake Bot API and loopback ports of the test
    // (no firewall question on Windows).
    let mine = format!(
        "# mine\nCCTG_BOT_API_URL=http://127.0.0.1:{api}\nCCTG_AGENT_LISTEN=127.0.0.1:{agent_port}\nCCTG_HOOK_LISTEN=127.0.0.1:{hook_port}\n"
    );
    std::fs::write(hub_dir.join("hub.env"), &mine).unwrap();
    let fake_state = root.dir("fake-service");
    let service_log = root.0.join("service.log");
    let run_key = format!(
        "HKCU\\Software\\cctg-install-e2e-{}\\Run",
        std::process::id()
    );
    let config_home = run.home.join(".config");
    run.env("CCTG_INSTALL_BASE_URL", &base)
        .env("CCTG_BOT_TOKEN", TOKEN)
        .env("FAKE_LOG", &service_log.to_string_lossy())
        .env("FAKE_DIR", &fake_state.to_string_lossy())
        .env("XDG_CONFIG_HOME", &config_home.to_string_lossy())
        .env("CCTG_INSTALL_RUN_KEY", &run_key);
    // The proxy comes only from hub.env.
    for var in [
        "HTTPS_PROXY",
        "https_proxy",
        "HTTP_PROXY",
        "http_proxy",
        "ALL_PROXY",
        "all_proxy",
        "NO_PROXY",
        "no_proxy",
    ] {
        run.env_remove(var);
    }
    // Windows: the installer's shell has its own proxy and state directory;
    // the hub, also at its first start, has the logon values of the registry
    // (start-hub.js), which name neither on a test machine.
    let shell_state = root.0.join("shell-state");
    if cfg!(windows) {
        run.env("HTTPS_PROXY", "http://127.0.0.1:9")
            .env("CCTG_STATE_DIR", &shell_state.to_string_lossy());
    }
    run.fake_program("systemctl", FAKE_SYSTEMCTL);
    run.fake_program("launchctl", FAKE_LAUNCHCTL);
    run.fake_program("claude", FAKE_CLAUDE);
    let mut guard = LocalHub {
        bin: run.cctg_dir().join("bin"),
        fake_state: fake_state.clone(),
        run_key: run_key.clone(),
        armed: true,
    };
    let hub_log = hub_dir.join("hub.log");
    let log = || std::fs::read_to_string(&hub_log).unwrap_or_default();

    let args = [
        "--hub",
        "--local",
        "--dir",
        &hub_dir_arg,
        "--yes",
        "--chat-id",
        "-1000000000001",
        "--users",
        "1001",
        "--public-host",
        "127.0.0.1",
    ];
    let first_args = [&args[..], &["--proxy", PROXY]].concat();
    let (output, text) = run.install(&first_args);
    assert!(
        output.status.success(),
        "{text}\n--- hub log ---\n{}",
        log()
    );
    assert!(!text.contains(TOKEN), "the token was printed: {text}");
    assert!(!text.contains(PROXY_MARK), "the proxy was printed: {text}");
    assert!(
        text.contains("hub started: bot and group checked"),
        "{text}"
    );
    assert!(listening(agent_port) && listening(hook_port), "{}", log());
    assert!(!log().contains('\u{1b}'), "colour codes in the log file");
    assert!(log().contains("HTTPS_PROXY of the env file"), "{}", log());
    assert!(!log().contains(PROXY_MARK), "{}", log());
    assert!(!shell_state.exists(), "the shell's CCTG_STATE_DIR was used");

    let env = std::fs::read_to_string(hub_dir.join("hub.env")).unwrap();
    assert!(env.starts_with(&mine), "{env}");
    for line in [
        format!("CCTG_PUBLIC_AGENT_ADDR=127.0.0.1:{agent_port}"),
        format!("CCTG_PUBLIC_HOOK_ADDR=127.0.0.1:{hook_port}"),
        "CCTG_CHAT_ID=-1000000000001".to_owned(),
        format!("HTTPS_PROXY={PROXY}"),
    ] {
        assert!(env.lines().any(|l| l == line), "{line} in {env}");
    }
    assert!(
        env.contains("CCTG_STATE_DIR='") && env.contains("CCTG_TLS_KEY='"),
        "{env}"
    );
    assert_eq!(env.matches("CCTG_AGENT_LISTEN").count(), 1, "{env}");
    let (_, pin) = cctg::tls::Acceptor::from_files(
        &hub_dir.join("tls").join("cert.pem"),
        &hub_dir.join("tls").join("key.pem"),
    )
    .expect("the hub's certificate");
    let pin = pin.to_string().replace(':', "").to_lowercase();
    // This machine's line: loopback with the pin.
    let local = text
        .lines()
        .find(|line| line.starts_with("curl -fsSL"))
        .expect("this machine's line");
    assert!(
        local.contains(&format!(
            "| sh -s -- --agent-addr 127.0.0.1:{agent_port} --hook-addr 127.0.0.1:{hook_port} --pin {pin} --join "
        )),
        "{local}"
    );

    // The autostart entry names this binary, hub.env and the log.
    let exe = run.exe();
    if cfg!(windows) {
        #[cfg(windows)]
        {
            // A short Run value (at most 260 characters): the paths are in
            // the launcher, in ASCII (WSH reads the ANSI code page).
            let value = run_value(&run_key).expect("the Run value");
            let native = |path: &Path| path.to_string_lossy().replace('/', "\\");
            assert!(
                value.ends_with(&format!(
                    "\\wscript.exe\" //B //Nologo \"{}\"",
                    native(&hub_dir.join("start-hub.js")),
                )),
                "{value}"
            );
            assert!(value.encode_utf16().count() <= 260, "{value}");
            let launcher = std::fs::read(hub_dir.join("start-hub.js")).unwrap();
            assert!(
                launcher.is_ascii(),
                "{}",
                String::from_utf8_lossy(&launcher)
            );
            let launcher = String::from_utf8(launcher).unwrap();
            let js = |path: &Path| {
                native(path)
                    .encode_utf16()
                    .map(|unit| match unit {
                        0x5c => "\\\\".to_owned(),
                        0x20..0x7f => char::from(unit as u8).to_string(),
                        _ => format!("\\u{unit:04x}"),
                    })
                    .collect::<String>()
            };
            assert!(
                launcher.contains(&format!(
                    "shell.Run(q(\"{}\") + \" supervise --env-file \" + q(\"{}\") + \" --log-file \" + q(\"{}\"), 0, false);",
                    js(&exe),
                    js(&hub_dir.join("hub.env")),
                    js(&hub_log)
                )),
                "{launcher}"
            );
            assert!(!launcher.contains(PROXY_MARK), "{launcher}");
        }
    } else if cfg!(target_os = "macos") {
        let plist = std::fs::read_to_string(
            run.home
                .join("Library/LaunchAgents/io.github.pockerhead.cctg-hub.plist"),
        )
        .unwrap();
        assert!(
            plist.contains(&format!("<string>{}</string>", exe.display()))
                && plist.contains(&format!("<string>{}</string>", hub_log.display()))
                && plist.contains("<string>--log-file</string>")
                && plist.contains("<key>RunAtLoad</key><true/>"),
            "{plist}"
        );
        assert!(!plist.contains(PROXY_MARK), "{plist}");
    } else {
        let unit = std::fs::read_to_string(run.home.join(".config/systemd/user/cctg-hub.service"))
            .unwrap();
        assert!(
            unit.contains(&format!(
                "ExecStart=\"{}\" supervise --env-file \"{}\" --log-file \"{}\"",
                exe.display(),
                hub_dir.join("hub.env").display(),
                hub_log.display()
            )) && unit.contains("WantedBy=default.target"),
            "{unit}"
        );
        assert!(!unit.contains(PROXY_MARK), "{unit}");
        let calls = std::fs::read_to_string(&service_log).unwrap();
        assert!(calls.contains("--user enable cctg-hub.service"), "{calls}");
    }

    // /join: a stranger gets nothing, the allowlisted user a line.
    bot.push_general(999, "/join");
    bot.push_general(USER, "/join");
    let lines = || -> Vec<(i64, Value)> {
        bot.sent()
            .into_iter()
            .filter(|(_, body)| {
                body["text"]
                    .as_str()
                    .is_some_and(|t| t.contains("curl -fsSL"))
            })
            .collect()
    };
    wait_until("the /join answer", || !lines().is_empty());
    std::thread::sleep(Duration::from_millis(500));
    let answers = lines();
    assert_eq!(answers.len(), 1, "{answers:?}");
    let (join_message, body) = &answers[0];
    assert!(
        body.get("message_thread_id").is_none_or(Value::is_null),
        "{body}"
    );
    assert_eq!(body["parse_mode"], "HTML", "{body}");
    let html = body["text"].as_str().unwrap();
    let line = html
        .split("<pre>")
        .nth(1)
        .and_then(|rest| rest.split("</pre>").next())
        .expect("the line in a code block");
    let tag = cctg::client::release().unwrap_or("main");
    let prefix = format!(
        "curl -fsSL https://raw.githubusercontent.com/pockerhead/cctg/{tag}/install.sh | sh -s -- "
    );
    let device_args: Vec<&str> = line
        .strip_prefix(&prefix)
        .unwrap_or_else(|| panic!("{line}"))
        .split(' ')
        .collect();
    let agent_addr = format!("127.0.0.1:{agent_port}");
    let hook_addr = format!("127.0.0.1:{hook_port}");
    assert_eq!(
        device_args[..7],
        [
            "--agent-addr",
            &agent_addr,
            "--hook-addr",
            &hook_addr,
            "--pin",
            &pin,
            "--join"
        ],
        "{line}"
    );

    // The line works: another machine installs with it.
    let mut device = Run {
        home: root.dir("device-home"),
        fake: run.fake.clone(),
        work: root.dir("device-work"),
        env: Vec::new(),
        removed: Vec::new(),
    };
    device.env("CCTG_INSTALL_BASE_URL", &base);
    let device_line = [&["--yes", "--host", "joined-box"][..], &device_args].concat();
    let (output, text) = device.install(&device_line);
    assert!(output.status.success(), "{text}");
    assert!(text.contains("this device is joined-box ("), "{text}");
    assert!(text.contains("took the secret"), "cctg doctor: {text}");
    let code = device_args[7];
    assert!(!text.contains(code), "{text}");
    // The /join message now says which device took its code.
    wait_until("the used mark on the /join message", || {
        bot.edits().iter().any(|edit| {
            edit["message_id"].as_i64() == Some(*join_message)
                && edit["text"]
                    .as_str()
                    .is_some_and(|t| t.contains("использован") && t.contains("joined-box"))
        })
    });

    // Again: the same binary, the hub started again on it, one supervisor.
    // The running hub is in its start checks (getMe fails, as with Telegram
    // out of reach) and fails them only while the installer waits: its
    // "Error: getMe failed" is not the new hub's, whose getMe is slow.
    let before = log().matches("hub started, polling").count();
    let mark = log().len();
    let since_mark = || log().get(mark..).unwrap_or_default().to_owned();
    bot.slow_get_me.store(1, Ordering::SeqCst);
    bot.get_me_failures.store(4, Ordering::SeqCst);
    std::fs::write(run.cctg_dir().join("bin").join("cctg.restart"), "").unwrap();
    wait_until("the running hub in its getMe retries", || {
        since_mark().contains("getMe failed; trying again")
    });
    let (output, text) = run.install(&args);
    assert!(
        output.status.success(),
        "{text}\n--- hub log ---\n{}",
        log()
    );
    assert!(
        since_mark().contains("Error: getMe failed"),
        "the old hub did not fail: {}",
        log()
    );
    assert!(text.contains("binary unchanged"), "{text}");
    let env = std::fs::read_to_string(hub_dir.join("hub.env")).unwrap();
    assert!(
        env.lines().any(|l| l == format!("HTTPS_PROXY={PROXY}")),
        "the proxy is kept: {env}"
    );
    // One hub more than before, plus one for each time the old hub ran out
    // of its getMe tries and the supervisor restarted it by itself: under
    // load that restart comes before the installer's request, and its hub
    // polls before the installer's restart replaces it (TASK-071).
    let polls = log().matches("hub started, polling").count() - before;
    let own_restarts = since_mark().matches("hub exited; restarting it").count();
    assert!(
        (1..=1 + own_restarts).contains(&polls),
        "{polls} hubs polled, {own_restarts} restarted by the supervisor: {}",
        log()
    );
    // The installer may have seen that restarted hub, and its own restart
    // can still be under way.
    wait_until("the hub listening", || listening(agent_port));

    // Uninstall: the hub stops, the autostart goes, the settings stay.
    let (output, text) = run.install(&["--hub", "--local", "--dir", &hub_dir_arg, "--uninstall"]);
    assert!(output.status.success(), "{text}");
    assert!(
        text.contains("the hub stopped"),
        "{text}\n--- hub log ---\n{}",
        log()
    );
    wait_until("the listeners to close", || !listening(agent_port));
    assert!(hub_dir.join("hub.env").exists() && hub_dir.join("tls").join("key.pem").exists());
    assert!(!run.cctg_dir().join("bin").join("cctg.local-hub").exists());
    if cfg!(windows) {
        #[cfg(windows)]
        {
            assert!(run_value(&run_key).is_none(), "{text}");
            assert!(!hub_dir.join("start-hub.js").exists());
        }
    } else if cfg!(target_os = "macos") {
        assert!(
            !run.home
                .join("Library/LaunchAgents/io.github.pockerhead.cctg-hub.plist")
                .exists()
        );
    } else {
        assert!(
            !run.home
                .join(".config/systemd/user/cctg-hub.service")
                .exists()
        );
        let calls = std::fs::read_to_string(&service_log).unwrap();
        assert!(
            calls.contains("--user disable --now cctg-hub.service"),
            "{calls}"
        );
    }
    guard.armed = false;
    drop(guard);
    drop(runtime);
}

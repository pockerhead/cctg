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
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::Value;

mod common;

/// Every character dotenv or sh could take for syntax (visible ASCII only:
/// the hub takes nothing else).
const SECRET: &str = r#"in'st$HOME#"x\y`z-0123456789"#;
const TOKEN: &str = "123456:install-e2e-token-value";
const EXE: &str = std::env::consts::EXE_SUFFIX;
const EVENTS: [&str; 10] = [
    "PermissionRequest",
    "PostToolUse",
    "PostToolUseFailure",
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
}

impl Run {
    fn new(root: &Root) -> Self {
        Self {
            home: root.dir("home-Иван"),
            fake: root.dir("fake-bin"),
            work: root.dir("work"),
            env: Vec::new(),
        }
    }

    fn env(&mut self, name: &str, value: &str) -> &mut Self {
        self.env.retain(|(key, _)| key != name);
        self.env.push((name.to_owned(), value.to_owned()));
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
        for (name, value) in &self.env {
            command.env(name, value);
        }
        let mut child = command.spawn().expect("start the shell");
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
        let status = shell
            .arg(run.wrapper())
            .arg("fix the bug")
            .env("PATH", path_with(&run.fake))
            .status()
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
            Command::new("codesign")
                .args(["--force", "-s", "-", "-i", "cctg.install-e2e.newer"])
                .arg(dist.join(asset()))
                .status()
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
        session
            .arg("run")
            .arg("--")
            .args(args)
            .env("CCTG_CLAUDE", program)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
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
        run.wrapper(),
        run.wrapper().with_file_name("claude-cctg.cmd"),
    ] {
        assert!(!gone.exists(), "{} left: {text}", gone.display());
    }
    assert!(aside.exists(), "a file not written by the script stays");
    assert!(run.cctg_dir().join("keep.txt").exists());
    untouched();
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
        "#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"$DOCKER_LOG\"\ncase \"$*\" in *' logs '*) echo 'hub-1  | INFO hub started, polling bot=x';; esac\nexit 0\n",
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
    let secret = value("CCTG_HUB_SECRET");
    assert!(
        cctg::wire::Secret::parse(&secret).is_ok() && secret.len() == 48,
        "{secret}"
    );
    assert_eq!(
        std::fs::read_to_string(hub_dir.join(".env"))
            .unwrap()
            .lines()
            .last(),
        Some("COMPOSE_FILE=compose.yml:compose.host.yml"),
        "a proxy on the host's loopback needs the host network"
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
    assert_eq!(
        line,
        format!(
            "curl -fsSL https://raw.githubusercontent.com/pockerhead/cctg/{}/install.sh \
             | CCTG_HUB_SECRET='{secret}' sh -s -- --hub-host hub.example.org --pin {pin}",
            release_tag()
        )
    );
    assert_eq!(
        text.matches(&secret).count(),
        1,
        "the secret only in the line"
    );
    let calls = std::fs::read_to_string(&docker_log).unwrap();
    assert!(
        calls.contains("compose pull hub") && calls.contains("compose up -d --force-recreate hub"),
        "{calls}"
    );

    // Again without the token: everything kept, the same pin and secret;
    // a compose.yml changed by hand (other ports) stays.
    run.env.retain(|(key, _)| key != "CCTG_BOT_TOKEN");
    let mine = std::fs::read_to_string(hub_dir.join("compose.yml"))
        .unwrap()
        .replace("\"47291:47291\"", "\"52191:47291\"");
    std::fs::write(hub_dir.join("compose.yml"), &mine).unwrap();
    let (output, text) = run.install(&[
        "--hub",
        "--yes",
        "--dir",
        &dir,
        "--public-host",
        "hub.example.org",
    ]);
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
        std::fs::read(deploy.join("compose.yml")).unwrap()
    );

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
fn the_script_has_unix_line_endings() {
    // A CRLF checkout (core.autocrlf on Windows) breaks `sh`; .gitattributes
    // keeps it LF.
    assert!(!install_sh().contains(&b'\r'));
}

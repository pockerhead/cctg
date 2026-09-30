//! The settings profile of a sandboxed folder:
//! `~/.cctg/claude/sandbox/<hash>.json` = cctg's `settings.json` plus the
//! strict Claude Code sandbox, the read block outside the working directory,
//! denials of the tools that lead out of the folder and the file-tool gate.
//! And [`retarget`], which points claude's arguments at the profile (or back
//! at `settings.json`) on every restart.
//!
//! Nothing an unsandboxed process consumes lives in the folder, where
//! sandboxed commands write: the gitconfig copy is in
//! `~/.cctg/sandbox/`. Tool caches in `<folder>/.cctg/sandbox/` reach Bash
//! only (`CLAUDE_ENV_FILE`, hook SessionStart), never the profile's `env`,
//! which also reaches hooks, the status line and MCP servers.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{Value, json};

use super::preflight::{self, Os, Probe, Refusal};
use super::{create_private_dir, paths, sandbox_home, write_if_changed};

/// claude options of a sandboxed start, in this order right before
/// `--settings`: no MCP servers but ours, no project or local settings (their
/// hooks and servers would run outside the sandbox), no Chrome.
pub const FLAGS: [&str; 4] = [
    "--strict-mcp-config",
    "--setting-sources",
    "user",
    "--no-chrome",
];

/// Linux: shared places sandboxed commands could still read. Not `/tmp` and
/// `/var/tmp`: probe P2 (WSL, 2.1.285) showed that `CLAUDE_CODE_TMPDIR` in
/// the profile does not move the sandbox temp (`TMPDIR` stays
/// `/tmp/claude-<uid>`), so temp is shared with the user's other sessions
/// (docs/sandbox.md).
// Probe P3 (WSL): all four are refused, P1, P5 and curl still work.
pub const LINUX_EXTRA_DENY_READ: &[&str] = &[
    "/run/user",
    "/run/docker.sock",
    "/var/run/docker.sock",
    "/run/containerd",
    "/run/podman",
];

/// macOS: empty until probe P3m passes with `/tmp`, `/private/tmp`,
/// `/var/tmp`, `/private/var/tmp` (no Mac probed: "not verified" in the doc).
// probe P3m: the four paths above when a Mac run passes.
pub const MACOS_EXTRA_DENY_READ: &[&str] = &[];

/// `disableSkillShellExecution`: a user skill's `` !`cmd` `` might run
/// outside the sandbox. Probe P6 was not run (it would put a skill into
/// the user's `~/.claude`), so it stays on.
pub const SKILL_SHELL_OFF: bool = true;

/// Tool homes under `<folder>/.cctg/sandbox/`.
pub const CACHE_DIRS: [&str; 6] = ["cargo", "cache", "data", "state", "npm", "go"];

/// `export` lines of the tool caches for Bash (hook SessionStart writes them
/// to `CLAUDE_ENV_FILE`): the caches live in the folder, `HOME` stays (rustup
/// finds `~/.rustup` through it). `None` for a folder that is not absolute or
/// has `'` or a control character (it could not be quoted safely).
// Probe P4 (WSL): they reach Bash, also after a `cd`.
pub fn cache_exports(folder: &str) -> Option<String> {
    if !quotable(folder) {
        return None;
    }
    let own = format!("{}/.cctg/sandbox", folder.trim_end_matches('/'));
    let lines = [
        ("CARGO_HOME", format!("{own}/cargo")),
        (
            "CARGO_TARGET_DIR",
            format!("{}/target", folder.trim_end_matches('/')),
        ),
        ("XDG_CACHE_HOME", format!("{own}/cache")),
        ("XDG_DATA_HOME", format!("{own}/data")),
        ("XDG_STATE_HOME", format!("{own}/state")),
        ("npm_config_cache", format!("{own}/npm")),
        ("GOPATH", format!("{own}/go")),
    ];
    Some(lines.iter().fold(String::new(), |mut out, (name, value)| {
        let _ = writeln!(out, "export {name}='{value}'");
        out
    }))
}

/// The git variables for Bash: the user-name-only gitconfig copy, no system
/// config, no password prompt. Probe P2e: Claude Code passes none of
/// `GIT_CONFIG_*` from the profile's `env` on, so they go this way.
pub fn git_exports(gitconfig: &str) -> Option<String> {
    quotable(gitconfig).then(|| {
        format!(
            "export GIT_CONFIG_GLOBAL='{gitconfig}'\nexport GIT_CONFIG_NOSYSTEM='1'\nexport GIT_TERMINAL_PROMPT='0'\n"
        )
    })
}

/// An absolute path that fits in single quotes of a shell line.
fn quotable(path: &str) -> bool {
    Path::new(path).is_absolute() && !path.chars().any(|c| c == '\'' || c.is_control())
}

/// The profile variable that names the gitconfig copy for [`git_exports`].
pub const GITCONFIG_VAR: &str = "CCTG_SANDBOX_GITCONFIG";

/// Appends [`cache_exports`] to `CLAUDE_ENV_FILE` when this session runs with
/// a profile. `None`: nothing to do.
pub fn export_caches(var: &impl Fn(&str) -> Option<String>) -> Option<std::io::Result<()>> {
    if var(super::ACTIVE_VAR).as_deref() != Some("1") {
        return None;
    }
    let file = var("CLAUDE_ENV_FILE").filter(|file| !file.trim().is_empty())?;
    let mut lines = cache_exports(&var("CLAUDE_PROJECT_DIR")?)?;
    if let Some(git) = var(GITCONFIG_VAR).and_then(|path| git_exports(&path)) {
        lines.push_str(&git);
    }
    Some((|| {
        let mut out = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(file)?;
        std::io::Write::write_all(&mut out, lines.as_bytes())
    })())
}

const GIT_TIMEOUT: Duration = Duration::from_secs(3);

/// Tools that lead out of the folder (TASK-087 F6): `/cd`, publishing a file
/// past cctg's checks, messages to the user's other sessions, cloud
/// routines, the desktop and the browser. Other projects' transcripts and
/// the prompt history need no rule: probe P1r showed the read block refuses
/// the Read tool there.
// probe P7 (WSL run 3): of the listed tools only DesignSync publishes
// outside; SendMessage, ListAgents, Artifact, SendUserFile, RemoteTrigger,
// computer-use, Chrome and claude.ai connectors are not listed.
// Probe P8: plan mode is not available (the env scrub forces the default
// permission mode), so EnterPlanMode needs no rule.
const DENY: &[&str] = &[
    "Cd",
    "Artifact",
    "SendUserFile",
    "SendMessage",
    "ListAgents",
    "RemoteTrigger",
    "mcp__computer-use",
    "mcp__claude-in-chrome",
    "DesignSync",
];

/// Variables sandboxed commands never see.
const SECRET_VARS: &[&str] = &[
    "CCTG_HUB_SECRET",
    "CCTG_JOIN_CODE",
    "CCTG_BOT_TOKEN",
    "GH_TOKEN",
    "GITHUB_TOKEN",
    "SSH_AUTH_SOCK",
];

/// First 16 hex digits of the sha256 of the folder as
/// [`crate::device::canonical_cwd`] spells it: `sandbox-check` and the
/// agent name one folder alike.
pub fn hash(folder: &Path) -> String {
    let spelled = crate::device::canonical_cwd(&folder.to_string_lossy());
    let digest = aws_lc_rs::digest::digest(&aws_lc_rs::digest::SHA256, spelled.as_bytes());
    digest.as_ref()[..8]
        .iter()
        .fold(String::new(), |mut out, byte| {
            let _ = write!(out, "{byte:02x}");
            out
        })
}

/// `<dir of base_settings>/sandbox/<hash>.json`.
pub fn profile_path(base_settings: &Path, folder: &Path) -> PathBuf {
    let dir = base_settings.parent().unwrap_or(Path::new(""));
    dir.join("sandbox").join(format!("{}.json", hash(folder)))
}

/// cctg's `settings.json` behind a `--settings` value: the file itself, or
/// the one two levels above a profile (`sandbox/<16 hex>.json`). `None` for
/// any other file (claude not started by our wrapper).
pub fn base_of(settings: &Path) -> Option<PathBuf> {
    let name = settings.file_name()?.to_str()?;
    if name == "settings.json" {
        return Some(settings.to_path_buf());
    }
    let parent = settings.parent()?;
    let stem = name.strip_suffix(".json")?;
    let profile = parent.file_name()? == "sandbox"
        && stem.len() == 16
        && stem.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'));
    profile.then(|| {
        parent
            .parent()
            .unwrap_or(Path::new(""))
            .join("settings.json")
    })
}

/// `~/.cctg/sandbox/read-dirs`: program directories sandboxed commands may
/// read (one per line, `#` comments, `~/` for home), resolved. An entry that
/// would open the home directory, cctg's or claude's private directories or
/// the folder itself (and with it its neighbours) is refused with its line.
pub fn read_dirs(home: &Path, private: &[PathBuf], folder: &Path) -> Result<Vec<PathBuf>, Refusal> {
    let text = match std::fs::read_to_string(sandbox_home(home).join("read-dirs")) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(_) => return Err(Refusal::Io("~/.cctg/sandbox/read-dirs")),
    };
    let mut dirs = Vec::new();
    for (index, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let path = match line.strip_prefix('~') {
            Some("") => home.to_path_buf(),
            Some(rest) if rest.starts_with('/') => home.join(&rest[1..]),
            _ => PathBuf::from(line),
        };
        let dir = (path.is_absolute() && !line.contains(['*', '?', '[']))
            .then(|| paths::canonical(&path))
            .flatten()
            .filter(|dir| {
                dir.is_dir()
                    && dir.parent().is_some()
                    && !paths::within(dir, home)
                    && !paths::within(dir, folder)
                    && !private.iter().any(|p| preflight::related(p, dir))
            });
        dirs.push(dir.ok_or(Refusal::BadReadDir(index + 1))?);
    }
    Ok(dirs)
}

/// Checks the device ([`preflight::check`]), makes the directories of the
/// folder and writes its profile (only when its bytes change: a new file time
/// restarts sessions on "Обновить"). `exe` is the cctg the gate hook runs.
/// Returns the profile's path.
pub fn prepare(
    probe: &dyn Probe,
    base_settings: &Path,
    folder: &Path,
    exe: &Path,
) -> Result<PathBuf, Refusal> {
    prepare_in(probe, base_settings, folder, exe, false)
}

/// A profile for the TASK-087 probes only: the same checks and contents,
/// but the file is `sandbox/probe-<hash>.json`, a name [`base_of`] does not
/// know, so no wrapper start and no restart ever takes it.
pub fn prepare_probe_run(
    probe: &dyn Probe,
    base_settings: &Path,
    folder: &Path,
    exe: &Path,
) -> Result<PathBuf, Refusal> {
    prepare_in(probe, base_settings, folder, exe, true)
}

fn prepare_in(
    probe: &dyn Probe,
    base_settings: &Path,
    folder: &Path,
    exe: &Path,
    probe_run: bool,
) -> Result<PathBuf, Refusal> {
    preflight::check(probe, folder)?;
    let home = probe.home().ok_or(Refusal::NoHome)?;
    let home = probe.canonical(&home).unwrap_or(home);
    let private = preflight::private_dirs(probe, &home);
    let allow_read = read_dirs(&home, &private, folder)?;
    let exe = gate_program(probe, exe)?;
    let mut settings: Value = std::fs::read(base_settings)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .filter(Value::is_object)
        .ok_or(Refusal::BaseSettings)?;

    let id = hash(folder);
    let sandbox = sandbox_home(&home);
    let git_dir = sandbox.join("git");
    create_private_dir(&git_dir).map_err(|_| Refusal::Io("~/.cctg/sandbox/git"))?;
    let gitconfig = git_dir.join(&id);
    write_if_changed(&gitconfig, user_gitconfig(probe).as_bytes())
        .map_err(|_| Refusal::Io("~/.cctg/sandbox/git"))?;
    folder_dirs(folder)?;

    let mut deny_read = vec!["~/.gitconfig".to_owned(), "~/.config/git".to_owned()];
    if let Some(state) = probe
        .var(crate::hub::config::STATE_VAR)
        .map(PathBuf::from)
        .filter(|dir| dir.is_absolute() && !paths::within(&home, dir))
    {
        deny_read.push(text(&state)?);
    }
    let extra = match probe.os() {
        Os::Linux => LINUX_EXTRA_DENY_READ,
        Os::MacOs => MACOS_EXTRA_DENY_READ,
        Os::Windows | Os::Other => &[],
    };
    deny_read.extend(extra.iter().map(|path| (*path).to_owned()));
    let mut allow_read = allow_read
        .iter()
        .map(|dir| text(dir))
        .collect::<Result<Vec<_>, _>>()?;
    allow_read.push(text(&gitconfig)?);

    merge(
        &mut settings,
        overlay(&text(&gitconfig)?, allow_read, deny_read, &exe),
    );
    let bytes = serde_json::to_vec_pretty(&settings).map_err(|_| Refusal::Io("профиль"))?;
    let mut path = profile_path(base_settings, folder);
    if probe_run {
        path.set_file_name(format!("probe-{}.json", hash(folder)));
    }
    if let Some(dir) = path.parent() {
        create_private_dir(dir).map_err(|_| Refusal::Io("профиль"))?;
    }
    write_if_changed(&path, &bytes).map_err(|_| Refusal::Io("профиль"))?;
    Ok(path)
}

fn overlay(gitconfig: &str, allow_read: Vec<String>, deny_read: Vec<String>, exe: &str) -> Value {
    let secrets: Vec<Value> = SECRET_VARS
        .iter()
        .map(|name| json!({ "name": name, "mode": "deny" }))
        .collect();
    let mut overlay = json!({
        // probe P2e (WSL run 3): these reach Bash, hooks and MCP; GIT_*
        // names do not (git_exports). Probe P2p: the scrub acts from here
        // (no secret in Bash); it also forces the permission mode to
        // default (plan, auto and acceptEdits are not available).
        "env": {
            "CCTG_SANDBOX": "1",
            "CLAUDE_CODE_SUBPROCESS_ENV_SCRUB": "1",
            "CCTG_SANDBOX_GITCONFIG": gitconfig,
            "ENABLE_CLAUDEAI_MCP_SERVERS": "false"
        },
        "sandbox": {
            "enabled": true,
            "failIfUnavailable": true,
            "allowUnsandboxedCommands": false,
            "enableWeakerNestedSandbox": false,
            // Review finding 2: user settings may set these; the profile's
            // value wins (--settings is above user settings). Any new
            // boolean that weakens the sandbox belongs here too.
            "allowAppleEvents": false,
            "enableWeakerNetworkIsolation": false,
            "autoAllowBashIfSandboxed": false,
            "filesystem": {
                "disabled": false,
                "allowRead": allow_read,
                "denyRead": deny_read
            },
            "network": { "allowAllUnixSockets": false, "allowLocalBinding": false },
            "credentials": { "envVars": secrets }
        },
        "permissions": {
            "blockReadsOutsideWorkingDirectories": true,
            "disableBypassPermissionsMode": "disable",
            "allow": ["WebFetch(domain:*)"],
            "deny": DENY
        },
        "disableClaudeAiConnectors": true,
        "autoMemoryEnabled": false,
        "hooks": { "PreToolUse": [{
            "matcher": "Write|Edit|MultiEdit|NotebookEdit|EnterWorktree",
            "hooks": [{
                "type": "command",
                "command": format!("\"{exe}\" sandbox-gate"),
                "timeout": 10
            }]
        }] }
    });
    if SKILL_SHELL_OFF {
        overlay["disableSkillShellExecution"] = Value::Bool(true);
    }
    overlay
}

/// Objects merge by key, arrays are appended to, anything else is replaced.
fn merge(base: &mut Value, overlay: Value) {
    match (base, overlay) {
        (Value::Object(base), Value::Object(overlay)) => {
            for (key, value) in overlay {
                match base.get_mut(&key) {
                    Some(slot) => merge(slot, value),
                    None => {
                        base.insert(key, value);
                    }
                }
            }
        }
        (Value::Array(base), Value::Array(overlay)) => base.extend(overlay),
        (slot, value) => *slot = value,
    }
}

/// The gate's program, resolved (the same bytes whoever writes the
/// profile) and safe inside `"..."` of a shell command line.
fn gate_program(probe: &dyn Probe, exe: &Path) -> Result<String, Refusal> {
    let exe = probe.canonical(exe).unwrap_or_else(|| exe.to_path_buf());
    let exe = text(&exe)?;
    let special =
        |c: char| matches!(c, '"' | '$' | '`') || c.is_control() || (cfg!(unix) && c == '\\');
    if exe.contains(special) {
        return Err(Refusal::Io("профиль (путь cctg с кавычками или $)"));
    }
    Ok(exe)
}

fn text(path: &Path) -> Result<String, Refusal> {
    path.to_str()
        .map(str::to_owned)
        .ok_or(Refusal::Io("профиль (путь не в UTF-8)"))
}

/// `[user]` with the name and email of the user's global git config, read
/// here, outside the sandbox. A value with a line break is dropped.
fn user_gitconfig(probe: &dyn Probe) -> String {
    let get = |key: &str| {
        let ran = probe.run("git", &["config", "--global", "--get", key], GIT_TIMEOUT)?;
        if ran.code != Some(0) {
            return None;
        }
        let value = ran.stdout.strip_suffix('\n').unwrap_or(&ran.stdout);
        let value = value.strip_suffix('\r').unwrap_or(value);
        (!value.is_empty() && !value.chars().any(char::is_control)).then(|| value.to_owned())
    };
    let entries: Vec<(&str, String)> = [("name", get("user.name")), ("email", get("user.email"))]
        .into_iter()
        .filter_map(|(key, value)| value.map(|value| (key, value)))
        .collect();
    if entries.is_empty() {
        return String::new();
    }
    let mut out = String::from("[user]\n");
    for (key, value) in entries {
        let value = value.replace('\\', "\\\\").replace('"', "\\\"");
        let _ = writeln!(out, "\t{key} = \"{value}\"");
    }
    out
}

/// `<folder>/.cctg/sandbox/` with the cache dirs and a `.gitignore` of `*`.
/// Every existing part from `<folder>/.cctg` down must be a real directory
/// (a sandboxed command could have put a link there).
fn folder_dirs(folder: &Path) -> Result<(), Refusal> {
    let base = folder.join(".cctg").join("sandbox");
    real_dir(&folder.join(".cctg"))?;
    real_dir(&base)?;
    for dir in CACHE_DIRS {
        real_dir(&base.join(dir))?;
    }
    let ignore = base.join(".gitignore");
    if std::fs::symlink_metadata(&ignore).is_ok_and(|meta| !meta.file_type().is_file()) {
        return Err(Refusal::FolderLink);
    }
    write_if_changed(&ignore, b"*\n").map_err(|_| Refusal::Io("<папка>/.cctg/sandbox"))?;
    Ok(())
}

fn real_dir(dir: &Path) -> Result<(), Refusal> {
    match std::fs::symlink_metadata(dir) {
        Ok(meta) if meta.file_type().is_dir() => Ok(()),
        Ok(_) => Err(Refusal::FolderLink),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            std::fs::create_dir(dir).map_err(|_| Refusal::Io("<папка>/.cctg/sandbox"))
        }
        Err(_) => Err(Refusal::Io("<папка>/.cctg/sandbox")),
    }
}

/// The first `--settings X` or `--settings=X`: its index, `X`, and whether
/// it is the `=` form.
fn find_settings(args: &[String]) -> Option<(usize, &str, bool)> {
    args.iter().enumerate().find_map(|(at, arg)| {
        if arg == "--settings" {
            args.get(at + 1).map(|value| (at, value.as_str(), false))
        } else {
            arg.strip_prefix("--settings=")
                .map(|value| (at, value, true))
        }
    })
}

/// cctg's `settings.json` behind claude's `--settings` ([`base_of`]).
pub fn settings_base(args: &[String]) -> Option<PathBuf> {
    find_settings(args).and_then(|(_, value, _)| base_of(Path::new(value)))
}

/// claude's arguments with `--settings` pointed at `profile` (and [`FLAGS`]
/// right before it), or back at cctg's `settings.json` with `None` (and the
/// flags gone). `None` when there is no `--settings` of ours. The rest keeps
/// its order.
pub fn retarget(args: &[String], profile: Option<&Path>) -> Option<Vec<String>> {
    let (at, value, inline) = find_settings(args)?;
    let base = base_of(Path::new(value))?;
    let target = profile.map_or(base, Path::to_path_buf);
    let target = target.to_str()?;
    let mut out = args[..at].to_vec();
    if out.len() >= FLAGS.len() && out[out.len() - FLAGS.len()..] == FLAGS {
        out.truncate(out.len() - FLAGS.len());
    }
    if profile.is_some() {
        out.extend(FLAGS.iter().map(|flag| (*flag).to_owned()));
    }
    if inline {
        out.push(format!("--settings={target}"));
    } else {
        out.push("--settings".to_owned());
        out.push(target.to_owned());
    }
    out.extend_from_slice(&args[at + if inline { 1 } else { 2 }..]);
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sandbox::preflight::tests::{Fake, home_and_folder};

    fn strings(list: &[&str]) -> Vec<String> {
        list.iter().map(|arg| (*arg).to_owned()).collect()
    }

    /// cctg's settings as install.sh writes them (shortened), and the exe.
    fn install(home: &Path) -> (PathBuf, PathBuf) {
        let conf = home.join(".cctg").join("claude");
        std::fs::create_dir_all(&conf).unwrap();
        let exe = home.join(".cctg").join("bin").join("cctg");
        std::fs::create_dir_all(exe.parent().unwrap()).unwrap();
        std::fs::write(&exe, b"").unwrap();
        let settings = conf.join("settings.json");
        std::fs::write(
            &settings,
            serde_json::to_vec_pretty(&json!({
                "statusLine": { "type": "command", "command": "\"cctg\" statusline" },
                "hooks": {
                    "SessionStart": [{ "hooks": [{ "type": "command", "command": "\"cctg\" hook SessionStart" }] }],
                    "PreToolUse": [
                        { "matcher": "AskUserQuestion", "hooks": [{ "type": "command", "command": "\"cctg\" hook PreToolUse" }] }
                    ]
                }
            }))
            .unwrap(),
        )
        .unwrap();
        (settings, exe)
    }

    fn with_git(fake: &mut Fake) {
        fake.answer("git config --global --get user.name", 0, "Ann \"A\" Lee\n");
        fake.answer(
            "git config --global --get user.email",
            0,
            "ann@example.org\n",
        );
    }

    #[test]
    fn the_profile_is_settings_plus_the_sandbox() {
        let (_dir, home, folder) = home_and_folder("profile-prepare");
        let (settings, exe) = install(&home);
        let tools = home.parent().unwrap().join("tools");
        std::fs::create_dir_all(&tools).unwrap();
        std::fs::create_dir_all(sandbox_home(&home)).unwrap();
        std::fs::write(
            sandbox_home(&home).join("read-dirs"),
            format!("# programs\n\n{}\n", tools.display()),
        )
        .unwrap();
        let mut fake = Fake::linux(&home);
        with_git(&mut fake);
        let base_bytes = std::fs::read(&settings).unwrap();

        let path = prepare(&fake, &settings, &folder, &exe).unwrap();
        assert_eq!(path, profile_path(&settings, &folder));
        assert_eq!(
            path.parent().unwrap(),
            home.join(".cctg").join("claude").join("sandbox")
        );
        let profile: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        let id = hash(&folder);
        let tmp = sandbox_home(&home).join("tmp").join(&id);
        let gitconfig = sandbox_home(&home).join("git").join(&id);
        let env = &profile["env"];
        assert_eq!(env[crate::sandbox::ACTIVE_VAR], "1");
        assert!(env.get("CLAUDE_CODE_TMPDIR").is_none(), "probe P2");
        assert_eq!(env["CLAUDE_CODE_SUBPROCESS_ENV_SCRUB"], "1");
        assert_eq!(env[GITCONFIG_VAR], gitconfig.to_str().unwrap());
        assert!(env.get("GIT_CONFIG_GLOBAL").is_none(), "probe P2e");
        assert_eq!(
            git_exports(gitconfig.to_str().unwrap()).unwrap(),
            format!(
                "export GIT_CONFIG_GLOBAL='{}'\nexport GIT_CONFIG_NOSYSTEM='1'\nexport GIT_TERMINAL_PROMPT='0'\n",
                gitconfig.display()
            )
        );
        assert_eq!(git_exports("relative"), None);
        assert_eq!(git_exports("/it's"), None);
        assert_eq!(env["ENABLE_CLAUDEAI_MCP_SERVERS"], "false");
        let sandbox = &profile["sandbox"];
        for key in ["enabled", "failIfUnavailable"] {
            assert_eq!(sandbox[key], true, "{key}");
        }
        for key in ["allowUnsandboxedCommands", "enableWeakerNestedSandbox"] {
            assert_eq!(sandbox[key], false, "{key}");
        }
        assert_eq!(sandbox["filesystem"]["disabled"], false);
        assert_eq!(
            sandbox["filesystem"]["allowRead"],
            json!([tools.to_str().unwrap(), gitconfig.to_str().unwrap()])
        );
        let deny_read = sandbox["filesystem"]["denyRead"].as_array().unwrap();
        assert_eq!(
            deny_read[..2],
            [json!("~/.gitconfig"), json!("~/.config/git")]
        );
        assert_eq!(deny_read.len(), 2 + LINUX_EXTRA_DENY_READ.len());
        assert!(deny_read.contains(&json!("/run/user")));
        assert!(!deny_read.contains(&json!("/tmp")), "probe P2");
        assert_eq!(sandbox["network"]["allowAllUnixSockets"], false);
        let secrets = sandbox["credentials"]["envVars"].as_array().unwrap();
        assert_eq!(secrets.len(), SECRET_VARS.len());
        assert!(secrets.contains(&json!({ "name": "CCTG_HUB_SECRET", "mode": "deny" })));
        let permissions = &profile["permissions"];
        assert_eq!(permissions["blockReadsOutsideWorkingDirectories"], true);
        assert_eq!(permissions["disableBypassPermissionsMode"], "disable");
        assert_eq!(permissions["allow"], json!(["WebFetch(domain:*)"]));
        assert_eq!(permissions["deny"], json!(DENY));
        assert_eq!(profile["disableClaudeAiConnectors"], true);
        assert_eq!(profile["autoMemoryEnabled"], false);
        assert_eq!(
            profile["disableSkillShellExecution"]
                .as_bool()
                .unwrap_or(false),
            SKILL_SHELL_OFF
        );

        // cctg's own settings stay; the gate comes after its hooks.
        assert_eq!(profile["statusLine"]["command"], "\"cctg\" statusline");
        assert_eq!(
            profile["hooks"]["SessionStart"].as_array().unwrap().len(),
            1
        );
        let pre = profile["hooks"]["PreToolUse"].as_array().unwrap();
        assert_eq!(pre.len(), 2);
        assert_eq!(pre[0]["matcher"], "AskUserQuestion");
        assert_eq!(
            pre[1]["matcher"],
            "Write|Edit|MultiEdit|NotebookEdit|EnterWorktree"
        );
        let exe = paths::canonical(&exe).unwrap();
        assert_eq!(
            pre[1]["hooks"][0]["command"],
            format!("\"{}\" sandbox-gate", exe.display())
        );
        assert_eq!(pre[1]["hooks"][0]["timeout"], 10);

        // Files and directories around it.
        assert!(!tmp.exists(), "probe P2: no own temp dir");
        assert_eq!(
            std::fs::read_to_string(&gitconfig).unwrap(),
            "[user]\n\tname = \"Ann \\\"A\\\" Lee\"\n\temail = \"ann@example.org\"\n"
        );
        let own = folder.join(".cctg").join("sandbox");
        for dir in CACHE_DIRS {
            assert!(own.join(dir).is_dir(), "{dir}");
        }
        assert_eq!(std::fs::read(own.join(".gitignore")).unwrap(), b"*\n");
        assert_eq!(
            std::fs::read(&settings).unwrap(),
            base_bytes,
            "the base is only read"
        );
    }

    /// Review finding 2: booleans of the user's settings that weaken the
    /// sandbox lose against the profile, whatever they say.
    #[test]
    fn weakening_user_booleans_are_forced_off() {
        let (_dir, home, folder) = home_and_folder("profile-weakening");
        let (settings, exe) = install(&home);
        let mut fake = Fake::linux(&home);
        fake.files.insert(
            home.join(".claude").join("settings.json"),
            r#"{"sandbox":{"allowAppleEvents":true,"enableWeakerNetworkIsolation":true,
                "autoAllowBashIfSandboxed":true,"network":{"allowLocalBinding":true}}}"#
                .to_owned(),
        );
        let path = prepare(&fake, &settings, &folder, &exe).unwrap();
        let profile: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        let sandbox = &profile["sandbox"];
        for key in [
            "allowAppleEvents",
            "enableWeakerNetworkIsolation",
            "autoAllowBashIfSandboxed",
        ] {
            assert_eq!(sandbox[key], false, "{key}");
        }
        assert_eq!(sandbox["network"]["allowLocalBinding"], false);
    }

    /// The probe-only profile is never a profile a start or restart takes.
    #[test]
    fn a_probe_run_profile_is_apart() {
        let (_dir, home, folder) = home_and_folder("profile-probe-run");
        let (settings, exe) = install(&home);
        let mut fake = Fake::linux(&home);
        let path = prepare_probe_run(&fake, &settings, &folder, &exe).unwrap();
        assert_eq!(
            path.file_name().unwrap().to_string_lossy(),
            format!("probe-{}.json", hash(&folder))
        );
        assert_eq!(base_of(&path), None, "no start or restart takes it");
        assert_ne!(path, profile_path(&settings, &folder));
        fake.answer("claude", 0, "2.1.1 (Claude Code)");
        assert!(prepare_probe_run(&fake, &settings, &folder, &exe).is_err());
    }

    #[test]
    fn an_unchanged_profile_keeps_its_file_time() {
        let (_dir, home, folder) = home_and_folder("profile-mtime");
        let (settings, exe) = install(&home);
        let fake = Fake::linux(&home);
        let path = prepare(&fake, &settings, &folder, &exe).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        let time = std::fs::metadata(&path).unwrap().modified().unwrap();
        let git = sandbox_home(&home).join("git").join(hash(&folder));
        let git_time = std::fs::metadata(&git).unwrap().modified().unwrap();
        assert_eq!(std::fs::read(&git).unwrap(), b"", "no git identity: empty");
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(prepare(&fake, &settings, &folder, &exe).unwrap(), path);
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        assert_eq!(std::fs::metadata(&path).unwrap().modified().unwrap(), time);
        assert_eq!(
            std::fs::metadata(&git).unwrap().modified().unwrap(),
            git_time
        );
    }

    #[test]
    fn mac_and_an_outside_state_dir() {
        let (dir, home, folder) = home_and_folder("profile-mac");
        let (settings, exe) = install(&home);
        let state = paths::canonical(dir.path()).unwrap().join("state");
        let mut fake = Fake::linux(&home);
        fake.os = Os::MacOs;
        fake.files
            .insert(PathBuf::from("/usr/bin/sandbox-exec"), String::new());
        fake.vars.insert(
            crate::hub::config::STATE_VAR.to_owned(),
            state.display().to_string(),
        );
        let path = prepare(&fake, &settings, &folder, &exe).unwrap();
        let profile: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        let mut expected = vec![json!("~/.gitconfig"), json!("~/.config/git")];
        expected.push(json!(state.to_str().unwrap()));
        expected.extend(MACOS_EXTRA_DENY_READ.iter().map(|p| json!(p)));
        assert_eq!(
            profile["sandbox"]["filesystem"]["denyRead"],
            json!(expected)
        );
    }

    #[test]
    fn refusals_write_nothing() {
        let (_dir, home, folder) = home_and_folder("profile-refused");
        let (settings, exe) = install(&home);
        let mut fake = Fake::linux(&home);
        fake.os = Os::Windows;
        assert_eq!(
            prepare(&fake, &settings, &folder, &exe),
            Err(Refusal::WindowsNotYet)
        );
        assert!(!sandbox_home(&home).exists());
        assert!(!folder.join(".cctg").exists());
        assert!(!profile_path(&settings, &folder).exists());

        let fake = Fake::linux(&home);
        std::fs::write(&settings, b"[1]").unwrap();
        assert_eq!(
            prepare(&fake, &settings, &folder, &exe),
            Err(Refusal::BaseSettings)
        );
        assert!(!profile_path(&settings, &folder).exists());
    }

    #[test]
    fn a_file_or_link_in_the_folder_is_not_followed() {
        let (_dir, home, folder) = home_and_folder("profile-link");
        let (settings, exe) = install(&home);
        let fake = Fake::linux(&home);
        std::fs::write(folder.join(".cctg"), b"").unwrap();
        assert_eq!(
            prepare(&fake, &settings, &folder, &exe),
            Err(Refusal::FolderLink)
        );
        std::fs::remove_file(folder.join(".cctg")).unwrap();
        std::fs::create_dir_all(folder.join(".cctg").join("sandbox").join("cargo")).unwrap();
        std::fs::create_dir_all(folder.join(".cctg").join("sandbox").join(".gitignore")).unwrap();
        assert_eq!(
            prepare(&fake, &settings, &folder, &exe),
            Err(Refusal::FolderLink)
        );
        #[cfg(unix)]
        {
            let outside = home.join("outside");
            std::fs::create_dir_all(&outside).unwrap();
            std::fs::remove_dir_all(folder.join(".cctg")).unwrap();
            std::os::unix::fs::symlink(&outside, folder.join(".cctg")).unwrap();
            assert_eq!(
                prepare(&fake, &settings, &folder, &exe),
                Err(Refusal::FolderLink)
            );
            std::fs::remove_file(folder.join(".cctg")).unwrap();
            std::fs::create_dir_all(folder.join(".cctg")).unwrap();
            std::os::unix::fs::symlink(&outside, folder.join(".cctg").join("sandbox")).unwrap();
            assert_eq!(
                prepare(&fake, &settings, &folder, &exe),
                Err(Refusal::FolderLink)
            );
            assert_eq!(std::fs::read_dir(&outside).unwrap().count(), 0);
        }
    }

    #[test]
    fn read_dirs_open_programs_only() {
        let (dir, home, folder) = home_and_folder("profile-read-dirs");
        let (settings, exe) = install(&home);
        let fake = Fake::linux(&home);
        let tools = paths::canonical(dir.path()).unwrap().join("tools");
        std::fs::create_dir_all(&tools).unwrap();
        std::fs::create_dir_all(home.join(".claude").join("projects")).unwrap();
        std::fs::create_dir_all(home.join(".rustup")).unwrap();
        std::fs::write(tools.join("file"), b"").unwrap();
        std::fs::create_dir_all(sandbox_home(&home)).unwrap();
        let root = home.ancestors().last().unwrap().to_path_buf();
        let refused = |line: &str| {
            std::fs::write(
                sandbox_home(&home).join("read-dirs"),
                format!("# comment\n{}\n{line}\n", tools.display()),
            )
            .unwrap();
            prepare(&fake, &settings, &folder, &exe)
        };
        for line in [
            "~".to_owned(),
            "~/".to_owned(),
            root.display().to_string(),
            home.parent().unwrap().display().to_string(),
            folder.display().to_string(),
            home.join(".claude").join("projects").display().to_string(),
            home.join(".cctg").display().to_string(),
            "relative/dir".to_owned(),
            tools.join("file").display().to_string(),
            tools.join("missing").display().to_string(),
            format!("{}*", tools.display()),
        ] {
            assert_eq!(refused(&line), Err(Refusal::BadReadDir(3)), "{line}");
        }
        assert!(refused("~/.rustup").is_ok(), "a program dir under home");
        assert!(refused("").is_ok());
        assert_eq!(
            read_dirs(&home, &[], &folder).unwrap(),
            vec![tools.clone()],
            "a blank line is skipped"
        );
    }

    #[test]
    fn base_and_profile_paths() {
        let base = Path::new("/h/.cctg/claude/settings.json");
        let folder = Path::new("/definitely/not/here/proj");
        let profile = profile_path(base, folder);
        assert_eq!(
            profile.parent().unwrap(),
            Path::new("/h/.cctg/claude/sandbox")
        );
        assert_eq!(hash(folder).len(), 16);
        assert_ne!(hash(folder), hash(Path::new("/definitely/not/here/proj2")));
        assert_eq!(base_of(base), Some(base.to_path_buf()));
        assert_eq!(base_of(&profile), Some(base.to_path_buf()));
        assert_eq!(base_of(Path::new("/h/sandbox/0123456789ABCDEF.json")), None);
        assert_eq!(base_of(Path::new("/h/sandbox/0123.json")), None);
        assert_eq!(base_of(Path::new("/h/other/0123456789abcdef.json")), None);
        assert_eq!(base_of(Path::new("/h/my-settings.json")), None);
    }

    #[test]
    fn retarget_points_at_the_profile_and_back() {
        let normal = strings(&[
            "--mcp-config",
            "/h/.cctg/claude/mcp.json",
            "--dangerously-load-development-channels",
            "server:cctg",
            "--settings",
            "/h/.cctg/claude/settings.json",
            "--model",
            "opus",
            "hello",
        ]);
        let profile = Path::new("/h/.cctg/claude/sandbox/0123456789abcdef.json");
        let sandboxed = retarget(&normal, Some(profile)).unwrap();
        assert_eq!(
            sandboxed,
            strings(&[
                "--mcp-config",
                "/h/.cctg/claude/mcp.json",
                "--dangerously-load-development-channels",
                "server:cctg",
                "--strict-mcp-config",
                "--setting-sources",
                "user",
                "--no-chrome",
                "--settings",
                profile.to_str().unwrap(),
                "--model",
                "opus",
                "hello",
            ])
        );
        assert_eq!(retarget(&sandboxed, Some(profile)).unwrap(), sandboxed);
        let back = retarget(&sandboxed, None).unwrap();
        assert_eq!(back.len(), normal.len());
        assert_eq!(back[..5], normal[..5]);
        // Joined by the host's separator: one path, maybe another spelling.
        assert_eq!(
            Path::new(&back[5]),
            Path::new(&normal[5]),
            "settings.json again"
        );
        assert_eq!(back[6..], normal[6..]);
        assert_eq!(
            retarget(&normal, None).unwrap(),
            normal,
            "normal stays normal"
        );

        let inline = strings(&["--settings=/h/.cctg/claude/settings.json", "-c"]);
        let inline_sandboxed = retarget(&inline, Some(profile)).unwrap();
        assert_eq!(inline_sandboxed[..4], FLAGS.map(str::to_owned));
        assert_eq!(
            inline_sandboxed[4],
            format!("--settings={}", profile.display())
        );
        assert_eq!(inline_sandboxed[5], "-c");

        assert_eq!(
            retarget(&strings(&["--model", "opus"]), Some(profile)),
            None
        );
        assert_eq!(
            retarget(&strings(&["--settings", "/x/my.json"]), Some(profile)),
            None,
            "not our settings"
        );
        assert_eq!(retarget(&strings(&["--settings"]), Some(profile)), None);
    }

    #[test]
    fn the_flags_survive_a_relaunch() {
        let normal = strings(&[
            "--mcp-config",
            "/h/.cctg/claude/mcp.json",
            "--dangerously-load-development-channels",
            "server:cctg",
            "--settings",
            "/h/.cctg/claude/settings.json",
            "fix it",
        ]);
        let profile = Path::new("/h/.cctg/claude/sandbox/0123456789abcdef.json");
        let sandboxed = retarget(&normal, Some(profile)).unwrap();
        let relaunched = crate::update::relaunch_args(&sandboxed, "sid-1");
        let mut expected = sandboxed[..sandboxed.len() - 1].to_vec();
        expected.extend(strings(&["--resume", "sid-1"]));
        assert_eq!(relaunched, expected, "flags and profile kept, prompt gone");
        assert_eq!(retarget(&relaunched, Some(profile)).unwrap(), relaunched);
        let normal_again = retarget(&relaunched, None).unwrap();
        assert!(!normal_again.iter().any(|arg| FLAGS.contains(&arg.as_str())));
        assert_eq!(
            normal_again[normal_again.len() - 2..],
            strings(&["--resume", "sid-1"])
        );
    }
}

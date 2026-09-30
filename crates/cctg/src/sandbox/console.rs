//! Which console lines from the topic the agent of a sandboxed session types
//! (TASK-087). Claude Code runs `!` lines and many slash commands outside
//! its sandbox (`/add-dir`, `/cd`, `/config`, `/permissions`, `/mcp`, ...),
//! so here only commands that act through tools (under the sandbox and the
//! gate) or change nothing outside the conversation are typed.
//!
//! Closed by default: an unknown name, a built-in command not on the safe
//! lists and every command of a newer Claude Code are refused. A skill counts
//! only when its file exists and no dangerous built-in starts with its name
//! or has a word that does (the command menu matches the start of a name or
//! of a word in it, ignoring case and `-`, `_`, `:`, and Enter runs the
//! highlighted match, docs commands). The whole line is looked at, not only
//! its start: a `!` anywhere, or a `/word` anywhere that could be taken for a
//! dangerous built-in, refuses it, so no skill can take a command along in
//! its arguments and run it later (review finding 1: `/loop 1m /add-dir ~`).
//! `@` anywhere is refused: an `@` file mention in the input box inserts a
//! file without a tool call (docs hooks).

use std::path::{Path, PathBuf};

/// Built-ins that change nothing outside the conversation.
pub const SAFE_BUILTINS: &[&str] = &[
    "compact", "clear", "cost", "usage", "context", "model", "effort", "status", "help", "btw",
    "recap",
];

/// Bundled skills: they act only through tools, under the sandbox and the
/// gate. Not `/loop`: it runs a prompt or a slash command later, typed by
/// Claude Code itself, past this check.
pub const BUNDLED_SKILLS: &[&str] = &[
    "batch",
    "claude-api",
    "code-review",
    "review",
    "dataviz",
    "debug",
    "design",
    "design-sync",
    "doctor",
    "fewer-permission-prompts",
    "run",
    "run-skill-generator",
    "simplify",
    "slides",
    "update-config",
    "verify",
    "workflow-authoring",
];

/// Every built-in command and alias of Claude Code 2.1.284 (docs commands,
/// copy in `maw/tasks/.../TASK-087/scratch/planner/docs/commands.md`). A
/// skill whose name matches one of them (without case and separators) is
/// never typed.
pub const BUILTIN_COMMANDS: &[&str] = &[
    "add-dir",
    "advisor",
    "agents",
    "android",
    "app",
    "allowed-tools",
    "artifacts",
    "auto-mode-setup",
    "autocompact",
    "autofix-pr",
    "background",
    "batch",
    "bg",
    "branch",
    "btw",
    "bug",
    "cd",
    "checkpoint",
    "checkup",
    "chrome",
    "claude-api",
    "clear",
    "code-review",
    "color",
    "compact",
    "config",
    "context",
    "continue",
    "copy",
    "cost",
    "dataviz",
    "debug",
    "deep-research",
    "design",
    "design-login",
    "design-sync",
    "desktop",
    "diff",
    "doctor",
    "effort",
    "exit",
    "export",
    "fast",
    "feedback",
    "fewer-permission-prompts",
    "focus",
    "fork",
    "goal",
    "heapdump",
    "help",
    "hooks",
    "ide",
    "import",
    "init",
    "insights",
    "install-github-app",
    "install-slack-app",
    "ios",
    "keybindings",
    "list-agents",
    "login",
    "logout",
    "loop",
    "mcp",
    "memory",
    "mobile",
    "model",
    "new",
    "output-style",
    "passes",
    "permissions",
    "plan",
    "plugin",
    "powerup",
    "pr-comments",
    "privacy-settings",
    "proactive",
    "quit",
    "radio",
    "rate-limit-options",
    "rc",
    "recap",
    "release-notes",
    "reload-plugins",
    "reload-skills",
    "remote-control",
    "remote-env",
    "rename",
    "reset",
    "resume",
    "review",
    "rewind",
    "routines",
    "run",
    "run-skill-generator",
    "sandbox",
    "schedule",
    "scroll-speed",
    "security-review",
    "settings",
    "setup-bedrock",
    "setup-vertex",
    "share",
    "simplify",
    "skill-doctor",
    "skills",
    "slides",
    "stats",
    "status",
    "statusline",
    "stickers",
    "stop",
    "subtask",
    "tasks",
    "team-onboarding",
    "teleport",
    "terminal-setup",
    "theme",
    "tui",
    "ultraplan",
    "ultrareview",
    "undo",
    "update-config",
    "upgrade",
    "usage",
    "usage-credits",
    "verify",
    "vim",
    "voice",
    "web-setup",
    "workflow-authoring",
    "workflows",
];

/// Up to this many skills can start one line (docs commands).
const MAX_CHAIN: usize = 6;
/// Plugin folders looked at when a `plugin:name` skill is looked for.
const MAX_PLUGIN_ENTRIES: usize = 4096;
const MAX_PLUGIN_DEPTH: usize = 6;

/// Skills and commands of the user's Claude Code config dir, looked up on
/// disk at each check (a few file lookups per command). Project skills do
/// not count: with `--setting-sources user` they are not loaded.
#[derive(Debug, Clone, Default)]
pub struct SkillIndex {
    pub config: Option<PathBuf>,
}

impl SkillIndex {
    pub fn from_env() -> Self {
        Self {
            config: super::claude_config_dir(&|name| std::env::var(name).ok()),
        }
    }

    /// `<config>/skills/<name>/SKILL.md`, `<config>/skills/synced/<name>/SKILL.md`
    /// or `<config>/commands/<name>.md`.
    fn user(&self, name: &str) -> bool {
        let Some(config) = &self.config else {
            return false;
        };
        let skills = config.join("skills");
        skills.join(name).join("SKILL.md").is_file()
            || skills.join("synced").join(name).join("SKILL.md").is_file()
            || config.join("commands").join(format!("{name}.md")).is_file()
    }

    /// `a:b`: a user command in a subfolder (`<config>/commands/a/b.md`) or
    /// skill `b` of plugin `a` somewhere under `<config>/plugins`.
    fn scoped(&self, scope: &str, name: &str) -> bool {
        let Some(config) = &self.config else {
            return false;
        };
        if config
            .join("commands")
            .join(scope)
            .join(format!("{name}.md"))
            .is_file()
        {
            return true;
        }
        let mut seen = 0;
        plugin_has(&config.join("plugins"), scope, name, 0, &mut seen)
    }
}

/// A directory named `plugin` below `dir` holds `skills/<name>/SKILL.md` or
/// `commands/<name>.md`, itself or in a version folder right in it
/// (`plugins/cache/<marketplace>/<plugin>/<version>/skills/...`). Links are
/// not followed; the walk is bounded.
fn plugin_has(dir: &Path, plugin: &str, name: &str, depth: usize, seen: &mut usize) -> bool {
    let has = |root: &Path| {
        root.join("skills").join(name).join("SKILL.md").is_file()
            || root.join("commands").join(format!("{name}.md")).is_file()
    };
    if depth > MAX_PLUGIN_DEPTH {
        return false;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return false;
    };
    for entry in entries.flatten() {
        *seen += 1;
        if *seen > MAX_PLUGIN_ENTRIES {
            return false;
        }
        if !entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            continue;
        }
        let path = entry.path();
        if entry.file_name() == plugin {
            let versions = std::fs::read_dir(&path).into_iter().flatten().flatten();
            if has(&path)
                || versions.take(MAX_PLUGIN_ENTRIES).any(|version| {
                    version.file_type().is_ok_and(|kind| kind.is_dir()) && has(&version.path())
                })
            {
                return true;
            }
        }
        if plugin_has(&path, plugin, name, depth + 1, seen) {
            return true;
        }
    }
    false
}

/// The agent may type `line` (a `!` line or a slash command, `@bot` already
/// taken off by the hub) into a sandboxed session's console.
pub fn allowed(line: &str, skills: &SkillIndex) -> bool {
    let line = line.trim();
    if !line.starts_with('/') || line.contains(['@', '!']) {
        return false;
    }
    if command_words(line).any(dangerous) {
        return false;
    }
    let names: Vec<&str> = line
        .split_whitespace()
        .take(MAX_CHAIN)
        .map_while(|word| word.strip_prefix('/'))
        .collect();
    !names.is_empty() && names.iter().all(|name| name_allowed(name, skills))
}

/// Every `/word` of `line` that could be read as a command: a `/` at the
/// start or after anything but a path character (`src/config` is a path,
/// `"/config` or `(/config` is not), and the name characters after it.
fn command_words(line: &str) -> impl Iterator<Item = &str> {
    let path_char = |c: char| c.is_alphanumeric() || matches!(c, '.' | '_' | '-' | '~' | '/');
    line.char_indices().filter_map(move |(at, c)| {
        let starts = c == '/'
            && line[..at]
                .chars()
                .next_back()
                .is_none_or(|before| !path_char(before));
        if !starts {
            return None;
        }
        let rest = &line[at + 1..];
        let end = rest
            .find(|c: char| !(c.is_alphanumeric() || matches!(c, '-' | '_' | ':')))
            .unwrap_or(rest.len());
        Some(&rest[..end]).filter(|word| !word.is_empty())
    })
}

fn name_allowed(name: &str, skills: &SkillIndex) -> bool {
    if SAFE_BUILTINS.contains(&name) || BUNDLED_SKILLS.contains(&name) {
        return true;
    }
    let plain = |part: &str| {
        !part.is_empty()
            && part
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    };
    if dangerous(name) {
        return false;
    }
    match name.split_once(':') {
        None if plain(name) => skills.user(name),
        Some((scope, rest)) if plain(scope) && plain(rest) => skills.scoped(scope, rest),
        _ => false,
    }
}

/// Typed as `/name`, the menu could highlight a built-in outside the safe
/// lists: `name` (case and `-`, `_`, `:` ignored) starts that built-in or
/// one of its words (`/add`, `/dir` -> `/add-dir`). A safe name itself is an
/// exact command (`/status`, not `/statusline`).
fn dangerous(name: &str) -> bool {
    if SAFE_BUILTINS.contains(&name) || BUNDLED_SKILLS.contains(&name) {
        return false;
    }
    let key = |text: &str| -> String {
        text.chars()
            .filter(|c| !matches!(c, '-' | '_' | ':'))
            .flat_map(char::to_lowercase)
            .collect()
    };
    let name = key(name);
    if name.is_empty() {
        return false;
    }
    BUILTIN_COMMANDS
        .iter()
        .filter(|builtin| !SAFE_BUILTINS.contains(builtin) && !BUNDLED_SKILLS.contains(builtin))
        .any(|builtin| {
            let words: Vec<&str> = builtin.split('-').collect();
            (0..words.len()).any(|from| key(&words[from..].concat()).starts_with(&name))
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hub::testdir::TempDir;

    fn index(name: &str) -> (TempDir, SkillIndex) {
        let dir = TempDir::new(name);
        let config = dir.path().join("claude");
        let skill = |path: PathBuf| {
            std::fs::create_dir_all(&path).unwrap();
            std::fs::write(path.join("SKILL.md"), b"---\nname: x\n---\n").unwrap();
        };
        skill(config.join("skills").join("myskill"));
        skill(config.join("skills").join("add-dir"));
        skill(config.join("skills").join("add_dir"));
        skill(config.join("skills").join("synced").join("shared"));
        skill(
            config
                .join("plugins")
                .join("cache")
                .join("market")
                .join("tools")
                .join("1.0.0")
                .join("skills")
                .join("lint"),
        );
        std::fs::create_dir_all(config.join("commands").join("front")).unwrap();
        std::fs::write(config.join("commands").join("front").join("comp.md"), b"x").unwrap();
        std::fs::write(config.join("commands").join("mine.md"), b"x").unwrap();
        // Review finding 6: user skills named like the start of a built-in
        // or of a word in it.
        for name in ["add", "dir", "perm", "sandb"] {
            skill(config.join("skills").join(name));
        }
        (
            dir,
            SkillIndex {
                config: Some(config),
            },
        )
    }

    #[test]
    fn only_safe_and_known_commands_are_typed() {
        let (_dir, skills) = index("console-policy");
        for yes in [
            "/compact",
            "/compact keep the plan",
            "/clear",
            "/model opus",
            "/code-review high",
            "/simplify",
            "/myskill",
            "/myskill do it",
            "/shared",
            "/mine",
            "/front:comp",
            "/tools:lint",
            "/myskill /simplify both",
        ] {
            assert!(allowed(yes, &skills), "{yes}");
        }
        for no in [
            "!ls",
            "!cat ~/.ssh/id_rsa",
            "/add-dir x",
            "/add-dir",
            "/adddir x",
            "/add_dir",
            "/ADD-DIR",
            "/cd ..",
            "/config",
            "/permissions",
            "/sandbox",
            "/hooks",
            "/mcp",
            "/plugin install x",
            "/resume",
            "/export out.txt",
            "/memory",
            "/login",
            "/agents",
            "/statusline",
            "/teleport",
            "/remote-control",
            "/rc",
            "/unknown",
            "/myskill /add-dir ..",
            "/myskill look @~/.ssh/id_rsa",
            "/compact @notes.md",
            "/tools:missing",
            "/add:dir",
            "/a:b:c",
            "/../skills/x",
            "/",
            "hello",
            "",
        ] {
            assert!(!allowed(no, &skills), "{no}");
        }
    }

    /// Review finding 1: a command or `!` anywhere in the line, also in the
    /// arguments of a skill that runs them later (`/loop 1m /add-dir ~`).
    #[test]
    fn a_command_hidden_in_the_arguments_is_refused() {
        let (_dir, skills) = index("console-hidden");
        for no in [
            "/loop 1m /add-dir x",
            "/loop 5m !cat ~/x",
            "/loop /add-dir x",
            "/loop",
            "/loop 1m check the build",
            "/compact keep /add-dir notes",
            "/myskill please run /config",
            "/myskill \"/permissions\"",
            "/myskill (/cd ..)",
            "/myskill say hi!",
            "/code-review /sandbox",
        ] {
            assert!(!allowed(no, &skills), "{no}");
        }
        for yes in [
            "/code-review src/config/x.rs",
            "/myskill fix ~/work/hooks",
            "/compact keep the /tmp notes",
        ] {
            assert!(allowed(yes, &skills), "{yes}");
        }
    }

    /// Review finding 6: the menu also highlights a built-in whose name or a
    /// word of it starts with what was typed; Enter would run it.
    #[test]
    fn a_skill_named_like_the_start_of_a_builtin_is_refused() {
        let (_dir, skills) = index("console-prefix");
        for no in ["/add", "/dir", "/perm", "/sandb", "/add some text"] {
            assert!(!allowed(no, &skills), "{no}");
        }
        for yes in ["/status", "/usage", "/design", "/myskill"] {
            assert!(allowed(yes, &skills), "{yes}");
        }
    }

    #[test]
    fn without_a_config_dir_only_the_lists_count() {
        let skills = SkillIndex::default();
        assert!(allowed("/compact", &skills));
        assert!(allowed("/verify", &skills));
        assert!(!allowed("/myskill", &skills));
        assert!(!allowed("/tools:lint", &skills));
    }

    #[test]
    fn the_lists_are_consistent() {
        for name in SAFE_BUILTINS.iter().chain(BUNDLED_SKILLS) {
            assert!(BUILTIN_COMMANDS.contains(name), "{name}");
            assert!(
                allowed(&format!("/{name}"), &SkillIndex::default()),
                "{name}"
            );
        }
        // Review finding 1: a skill that runs prompts or commands later is
        // no tool-only skill.
        assert!(!BUNDLED_SKILLS.contains(&"loop"));
    }
}

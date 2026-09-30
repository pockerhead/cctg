//! `cctg sandbox-gate`: the sync `PreToolUse` hook of a sandbox profile for
//! `Write`, `Edit`, `MultiEdit`, `NotebookEdit` and `EnterWorktree`. The file
//! tools run in Claude Code itself, outside the Bash sandbox; this gate keeps
//! their writes inside the session folder and off its protected files.
//!
//! Fail closed: a deny, bad input, a missing folder, bad arguments and a
//! panic all exit 2, the one code that blocks the call (exit 0 without JSON
//! and exit 1 let it through, docs hooks "Exit code 2"). This is why the
//! gate is its own subcommand and not a `cctg hook` event, which always exits
//! 0. No network, no hub; stderr carries a fixed line, never a path.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Deserialize;

use super::paths;

/// Exit code that blocks the tool call.
pub const DENY_CODE: i32 = 2;
/// Shown to Claude with a deny.
pub const DENY_MESSAGE: &str = "cctg sandbox: writing outside the session folder or into its \
     protected files (.claude, .git, .mcp.json, shell and IDE configs) is not allowed in this folder";
const STDIN_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Allow,
    Deny,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct Input {
    tool_name: String,
    tool_input: ToolInput,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct ToolInput {
    file_path: Option<String>,
    notebook_path: Option<String>,
    path: Option<String>,
}

/// Decides one hook input; `var` is the process environment.
pub fn decide(input: &[u8], var: impl Fn(&str) -> Option<String>) -> Verdict {
    if var(super::ACTIVE_VAR).as_deref() != Some("1") {
        // The gate stands only in a profile; this is a second guard.
        return Verdict::Allow;
    }
    let Some(root) = var("CLAUDE_PROJECT_DIR")
        .map(PathBuf::from)
        .filter(|root| root.is_absolute() && root.is_dir())
    else {
        return Verdict::Deny;
    };
    let Ok(input) = serde_json::from_slice::<Input>(input) else {
        return Verdict::Deny;
    };
    let target = match input.tool_name.as_str() {
        "Write" | "Edit" | "MultiEdit" => input.tool_input.file_path,
        "NotebookEdit" => input.tool_input.notebook_path,
        "EnterWorktree" => {
            return match input.tool_input.path.filter(|path| !path.is_empty()) {
                None => Verdict::Allow,
                Some(path) if worktree(&root, Path::new(&path)) => Verdict::Allow,
                Some(_) => Verdict::Deny,
            };
        }
        _ => return Verdict::Allow,
    };
    let Some(target) = target.filter(|path| !path.is_empty()) else {
        return Verdict::Deny;
    };
    let target = Path::new(&target);
    let in_folder = paths::contains(&root, target) && !paths::protected(&root, target);
    // probe P8: plan mode writes its plan here.
    let plan =
        super::claude_config_dir(&var).is_some_and(|config| paths::plan_file(&config, target));
    if in_folder || plan {
        Verdict::Allow
    } else {
        Verdict::Deny
    }
}

/// `path` is a worktree below `<root>/.claude/worktrees/` (probe P9).
fn worktree(root: &Path, path: &Path) -> bool {
    let (Some(base), Some(path)) = (
        paths::canonical(root).map(|root| root.join(".claude").join("worktrees")),
        paths::resolve(root, path),
    ) else {
        return false;
    };
    paths::within(&base, &path) && !paths::within(&path, &base)
}

/// The subcommand: reads stdin, decides, answers with the exit code.
pub fn run() -> i32 {
    let Some(input) = crate::hook::read_stdin(crate::hook::MAX_STDIN, STDIN_TIMEOUT) else {
        eprintln!("{DENY_MESSAGE}");
        return DENY_CODE;
    };
    match decide(&input, |name| std::env::var(name).ok()) {
        Verdict::Allow => 0,
        Verdict::Deny => {
            eprintln!("{DENY_MESSAGE}");
            DENY_CODE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hub::testdir::TempDir;
    use serde_json::json;

    struct Place {
        _dir: TempDir,
        root: PathBuf,
        config: PathBuf,
    }

    fn place(name: &str) -> Place {
        let dir = TempDir::new(name);
        let base = paths::canonical(dir.path()).unwrap();
        let root = base.join("proj");
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::create_dir_all(root.join(".claude").join("worktrees").join("w")).unwrap();
        let config = base.join("claude");
        std::fs::create_dir_all(config.join("plans")).unwrap();
        std::fs::create_dir_all(base.join("outside")).unwrap();
        Place {
            _dir: dir,
            root,
            config,
        }
    }

    impl Place {
        fn decide(&self, tool: &str, input: serde_json::Value) -> Verdict {
            self.decide_with(tool, input, Some("1"))
        }

        fn decide_with(
            &self,
            tool: &str,
            input: serde_json::Value,
            active: Option<&str>,
        ) -> Verdict {
            let body = json!({ "tool_name": tool, "tool_input": input }).to_string();
            let root = self.root.display().to_string();
            let config = self.config.display().to_string();
            decide(body.as_bytes(), |name| match name {
                "CCTG_SANDBOX" => active.map(str::to_owned),
                "CLAUDE_PROJECT_DIR" => Some(root.clone()),
                "CLAUDE_CONFIG_DIR" => Some(config.clone()),
                _ => None,
            })
        }
    }

    #[test]
    fn writes_stay_in_the_folder_and_off_its_protected_files() {
        let p = place("gate-writes");
        let file = |path: &Path| json!({ "file_path": path.display().to_string() });
        assert_eq!(
            p.decide("Write", file(&p.root.join("src").join("a.rs"))),
            Verdict::Allow
        );
        assert_eq!(
            p.decide("Edit", json!({ "file_path": "src/b.rs" })),
            Verdict::Allow
        );
        assert_eq!(
            p.decide("MultiEdit", file(&p.root.join("new").join("c.rs"))),
            Verdict::Allow
        );
        let outside = p.root.join("..").join("outside").join("x");
        for denied in [
            outside,
            p.root.join(".claude").join("settings.json"),
            p.root.join(".git").join("hooks").join("x"),
            p.root.join(".mcp.json"),
            p.root.join(".bashrc"),
        ] {
            assert_eq!(
                p.decide("Write", file(&denied)),
                Verdict::Deny,
                "{}",
                denied.display()
            );
        }
        assert_eq!(
            p.decide(
                "Write",
                file(
                    &p.root
                        .join(".claude")
                        .join("worktrees")
                        .join("w")
                        .join("a.rs")
                )
            ),
            Verdict::Allow,
            "a worktree is a working folder"
        );
        assert_eq!(
            p.decide("Write", file(&p.config.join("plans").join("plan.md"))),
            Verdict::Allow
        );
        assert_eq!(
            p.decide("Write", file(&p.config.join("settings.json"))),
            Verdict::Deny
        );
        assert_eq!(p.decide("Write", json!({})), Verdict::Deny, "no path");
        assert_eq!(p.decide("Write", json!({ "file_path": "" })), Verdict::Deny);
        let notebook = p.root.join("n.ipynb").display().to_string();
        assert_eq!(
            p.decide("NotebookEdit", json!({ "notebook_path": notebook })),
            Verdict::Allow
        );
        assert_eq!(
            p.decide("NotebookEdit", json!({ "file_path": notebook })),
            Verdict::Deny
        );
        assert_eq!(
            p.decide("Bash", json!({ "command": "rm -rf ~" })),
            Verdict::Allow,
            "not the gate's"
        );
        assert_eq!(
            p.decide("Read", file(Path::new("/etc/passwd"))),
            Verdict::Allow
        );
    }

    #[test]
    fn worktrees_only_below_the_folder() {
        let p = place("gate-worktree");
        assert_eq!(p.decide("EnterWorktree", json!({})), Verdict::Allow);
        assert_eq!(
            p.decide("EnterWorktree", json!({ "name": "x" })),
            Verdict::Allow
        );
        assert_eq!(
            p.decide("EnterWorktree", json!({ "path": "../outside" })),
            Verdict::Deny
        );
        assert_eq!(
            p.decide("EnterWorktree", json!({ "path": "src" })),
            Verdict::Deny
        );
        assert_eq!(
            p.decide("EnterWorktree", json!({ "path": ".claude/worktrees" })),
            Verdict::Deny
        );
        let w = p.root.join(".claude").join("worktrees").join("w");
        assert_eq!(
            p.decide("EnterWorktree", json!({ "path": w.display().to_string() })),
            Verdict::Allow
        );
    }

    #[test]
    fn anything_unclear_is_a_deny() {
        let p = place("gate-unclear");
        let root = p.root.display().to_string();
        let active = |name: &str| match name {
            "CCTG_SANDBOX" => Some("1".to_owned()),
            "CLAUDE_PROJECT_DIR" => Some(root.clone()),
            _ => None,
        };
        assert_eq!(decide(b"{broken", active), Verdict::Deny);
        assert_eq!(
            decide(
                br#"{"tool_name":"Write","tool_input":{"file_path":7}}"#,
                active
            ),
            Verdict::Deny
        );
        let body = br#"{"tool_name":"Write","tool_input":{"file_path":"src/a.rs"}}"#;
        assert_eq!(decide(body, active), Verdict::Allow);
        let no_root = |name: &str| (name == "CCTG_SANDBOX").then(|| "1".to_owned());
        assert_eq!(decide(body, no_root), Verdict::Deny);
        let relative_root = |name: &str| match name {
            "CCTG_SANDBOX" => Some("1".to_owned()),
            "CLAUDE_PROJECT_DIR" => Some("proj".to_owned()),
            _ => None,
        };
        assert_eq!(decide(body, relative_root), Verdict::Deny);
        let outside = json!({ "file_path": p.root.join("..").join("x").display().to_string() });
        assert_eq!(
            p.decide_with("Write", outside.clone(), None),
            Verdict::Allow,
            "not a sandbox session"
        );
        assert_eq!(p.decide_with("Write", outside, Some("0")), Verdict::Allow);
        assert!(!DENY_MESSAGE.contains('/') && !DENY_MESSAGE.contains('\\'));
    }
}

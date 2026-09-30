//! The Windows shell-prefix broker's parse of its one argument (`$1`).
//!
//! On native Windows a sandboxed folder's profile points
//! `CLAUDE_CODE_SHELL_PREFIX` at the shim `~/.cctg/bin/cctg-sandbox-exec`,
//! which calls `cctg sandbox-exec "$1"`. Claude Code hands the prefix every
//! Bash tool call (a long `source … && eval <cmd> && pwd -P >| …` string, full
//! of shell metacharacters) and — probe P0c (TASK-089) — the launch of a
//! stdio MCP server, but NOT exec-form hooks or the status line command run
//! straight. So the broker must pass straight through only its own stdio MCP
//! server (`cctg agent`) and status line (`cctg statusline`); everything else,
//! every Bash form, goes into the sandbox.
//!
//! [`own_call`] is that decision, and it is pure: it tokenises `$1` with a
//! strict grammar (no shell), so any Bash command — which always carries a
//! space-run, a quote, `&&`, `$(…)` or a redirection the grammar rejects — is
//! never mistaken for one of our two calls. Cross-platform so its table of
//! vectors runs in every CI target.

/// Which of cctg's own subcommands `$1` is, when it is one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Own {
    Agent,
    Statusline,
}

/// `Some((program, own))` only when `line` is exactly `<program> agent` or
/// `<program> statusline` under the grammar below; `None` (sandbox it)
/// otherwise. `program` is the unquoted first token, an absolute path with
/// forward slashes.
///
/// Grammar (anything else is `None`):
/// - exactly two tokens separated by one or more U+0020 spaces; leading and
///   trailing spaces are allowed; any other whitespace or control character
///   (`\t`, `\r`, `\n`, U+00A0, …) anywhere makes it `None`;
/// - a token is a bare run of `[A-Za-z0-9_./:-]`, or a single-quoted `'…'`
///   with no `'` inside, or a double-quoted `"…"` with no `"`, `$`, `` ` `` or
///   `\` inside; a quote pair spans the whole token (`"a"b` is `None`);
/// - the first token, unquoted, is non-empty and absolute (`X:/…`; a
///   backslash cannot occur, so only forward slashes);
/// - the second token, unquoted, is exactly `agent` or `statusline`.
pub fn own_call(line: &str) -> Option<(String, Own)> {
    // One space is the only allowed whitespace; any control character or
    // other whitespace (a Bash `$1` always has a tab, newline or U+00A0
    // somewhere, and never only our two tokens) rejects the whole line.
    if line
        .chars()
        .any(|c| c != ' ' && (c.is_control() || c.is_whitespace()))
    {
        return None;
    }
    let tokens = tokenize(line)?;
    let [program, sub] = tokens.as_slice() else {
        return None;
    };
    if !absolute(program) {
        return None;
    }
    match sub.as_str() {
        "agent" => Some((program.clone(), Own::Agent)),
        "statusline" => Some((program.clone(), Own::Statusline)),
        _ => None,
    }
}

/// The unquoted tokens of `line` under the grammar, or `None` when a token is
/// malformed (an unterminated quote, forbidden characters in a quote, or a
/// token that does not end at a space).
fn tokenize(line: &str) -> Option<Vec<String>> {
    let bytes = line.as_bytes();
    let mut at = 0;
    let mut tokens = Vec::new();
    while at < bytes.len() {
        // Skip the space run between tokens (and any leading spaces).
        if bytes[at] == b' ' {
            at += 1;
            continue;
        }
        let (value, next) = token(bytes, at)?;
        // A token must end at a space or at the end of the line.
        if next < bytes.len() && bytes[next] != b' ' {
            return None;
        }
        tokens.push(value);
        at = next;
    }
    Some(tokens)
}

/// One token starting at `start` (not a space): its unquoted value and the
/// index just past it. `None` for a malformed token.
fn token(bytes: &[u8], start: usize) -> Option<(String, usize)> {
    match bytes[start] {
        b'\'' => {
            let end = bytes[start + 1..].iter().position(|&b| b == b'\'')?;
            let content = &bytes[start + 1..start + 1 + end];
            Some((str_of(content)?, start + 1 + end + 1))
        }
        b'"' => {
            let end = bytes[start + 1..].iter().position(|&b| b == b'"')?;
            let content = &bytes[start + 1..start + 1 + end];
            if content.iter().any(|&b| matches!(b, b'$' | b'`' | b'\\')) {
                return None;
            }
            Some((str_of(content)?, start + 1 + end + 1))
        }
        _ => {
            let len = bytes[start..].iter().take_while(|&&b| bare(b)).count();
            if len == 0 {
                return None;
            }
            Some((str_of(&bytes[start..start + len])?, start + len))
        }
    }
}

/// A byte allowed in a bare (unquoted) token.
fn bare(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'/' | b':' | b'-')
}

/// The bytes as a `String` (they came from `&str`, so they are valid UTF-8).
fn str_of(bytes: &[u8]) -> Option<String> {
    std::str::from_utf8(bytes).ok().map(str::to_owned)
}

/// `X:/…`: a drive-letter path with a forward slash. OS-independent (the
/// grammar forbids backslashes, so this is the only absolute shape).
fn absolute(path: &str) -> bool {
    let bytes = path.as_bytes();
    bytes.len() >= 3 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' && bytes[2] == b'/'
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_our_two_exact_forms_pass_through() {
        let agent = "C:/Users/u/.cctg/bin/cctg.exe";
        assert_eq!(
            own_call(&format!("\"{agent}\" agent")),
            Some((agent.to_owned(), Own::Agent))
        );
        assert_eq!(
            own_call(&format!("'{agent}' 'agent'")),
            Some((agent.to_owned(), Own::Agent))
        );
        assert_eq!(
            own_call(&format!("{agent} statusline")),
            Some((agent.to_owned(), Own::Statusline))
        );
        // A quoted program path with a space (a real user folder).
        let spaced = "C:/Users/John Doe/.cctg/bin/cctg.exe";
        assert_eq!(
            own_call(&format!("\"{spaced}\" agent")),
            Some((spaced.to_owned(), Own::Agent))
        );
        // Leading and trailing spaces are allowed.
        assert_eq!(
            own_call(&format!("  {agent} agent  ")),
            Some((agent.to_owned(), Own::Agent))
        );
        // Several spaces between the tokens.
        assert_eq!(
            own_call(&format!("{agent}    statusline")),
            Some((agent.to_owned(), Own::Statusline))
        );
    }

    #[test]
    fn every_bash_form_and_trick_is_sandboxed() {
        let p = "C:/Users/u/.cctg/bin/cctg.exe";
        // The P0a hook marker and a real Bash `$1` (probe result.txt).
        let bash = "source /c/Users/u/.claude/shell-snapshots/snapshot-bash-1.sh 2>/dev/null \
             || true && eval 'echo hi' < /dev/null && pwd -P >| /c/Users/u/Temp/claude-7c2f-cwd";
        for line in [
            "cctg089-hook-marker arg2 arg3",
            bash,
            &format!("\"{p}\" agent; cat x"),
            &format!("\"{p}\" agent && x"),
            &format!("\"{p}\" agent | x"),
            &format!("\"{p}\" $(x)"),
            &format!("\"{p}\" `x`"),
            &format!("\"{p}\" agent\n"),
            &format!("\"{p}\"\tagent"),
            &format!("\"{p}\" agent extra"),
            &format!("\"{p}\" hook SessionStart"),
            &format!("\"{p}\"agent"),
            &format!("\"{p}\" \"agent\"x"),
            r#""C:\x\cctg.exe" agent"#,
            "relative/cctg agent",
            "cctg.exe agent",
            &format!("\"{p}\" agent\u{a0}"),
            &format!("'{p}' agent'"),
            &format!("\"{p} agent"),
            "",
            "   ",
            p,
        ] {
            assert_eq!(own_call(line), None, "{line:?}");
        }
    }

    #[test]
    fn a_program_must_be_absolute_and_the_sub_exact() {
        let p = "C:/x/cctg.exe";
        assert_eq!(own_call(&format!("{p} agentx")), None);
        assert_eq!(own_call(&format!("{p} Agent")), None);
        assert_eq!(own_call(&format!("{p} run")), None);
        assert_eq!(own_call("/usr/bin/cctg agent"), None, "posix path");
        assert!(own_call(&format!("{p} agent")).is_some());
    }
}

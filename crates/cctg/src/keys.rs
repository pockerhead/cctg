//! Presses a key in the console of the agent's Claude Code process
//! (TASK-029): Esc, as when typed in the terminal. Since TASK-040 also types
//! `/exit` for a restart ([`type_exit`], which reads the input box back
//! first) and serves `cctg run` its own console ([`visible_lines`],
//! [`write_text`]); since TASK-043 any one-line command from the topic
//! ([`type_line`], the same safe typing).
//!
//! Channels have no command for it, so on Windows the agent (a child of
//! that claude) attaches to its console and writes the key events into the
//! console input: `FreeConsole` + `AttachConsole(claude pid)` + `CONIN$` +
//! `WriteConsoleInputW`, then `FreeConsole` again. A console is attached per
//! process, so one press at a time. The agent's stdio are pipes and stay
//! untouched. Elsewhere nothing is supported yet and the agent does not
//! announce the capability.
//!
//! A successful write says only that the key events are in the console
//! input buffer (behind any input that waits there), not that Claude Code
//! read them or that its turn ended.

use crate::wire::ConsoleKey;

/// Whether this build can press keys at all.
pub const SUPPORTED: bool = cfg!(windows);

/// `(virtual key, scan code, character, control key state)` of `key`.
pub fn key_event(key: ConsoleKey) -> (u16, u16, u16, u32) {
    match key {
        ConsoleKey::Interrupt => (0x1B, 0x01, 0x1B, 0),
    }
}

/// Writes `key` (down and up) into the console input of process
/// `claude_pid`. Blocking and short; `false` when any step failed.
#[cfg(windows)]
pub fn press(claude_pid: u32, key: ConsoleKey) -> bool {
    use windows_sys::Win32::Foundation::{
        CloseHandle, GENERIC_READ, GENERIC_WRITE, INVALID_HANDLE_VALUE,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
    };
    use windows_sys::Win32::System::Console::{
        AttachConsole, FreeConsole, INPUT_RECORD, KEY_EVENT, SetConsoleCtrlHandler,
        WriteConsoleInputW,
    };

    let _one = one_at_a_time();
    let (vk, scan, ch, state) = key_event(key);
    let mut records = [INPUT_RECORD::default(), INPUT_RECORD::default()];
    for (record, down) in records.iter_mut().zip([1, 0]) {
        record.EventType = KEY_EVENT as u16;
        // SAFETY: `KeyEvent` is the union member that `EventType = KEY_EVENT`
        // selects; every field is plain data.
        let event = unsafe { &mut record.Event.KeyEvent };
        event.bKeyDown = down;
        event.wRepeatCount = 1;
        event.wVirtualKeyCode = vk;
        event.wVirtualScanCode = scan;
        event.uChar.UnicodeChar = ch;
        event.dwControlKeyState = state;
    }
    let conin: Vec<u16> = "CONIN$\0".encode_utf16().collect();
    // SAFETY: plain Win32 calls with valid arguments: a NUL-terminated wide
    // string, a live array of two initialized records and its length, a
    // handle that is closed once. While attached, Ctrl+C in that console is
    // ignored by this process (`SetConsoleCtrlHandler(None, 1)`), so a user's
    // Ctrl+C cannot end the agent.
    unsafe {
        SetConsoleCtrlHandler(None, 1);
        FreeConsole();
        if AttachConsole(claude_pid) == 0 {
            return false;
        }
        let input = CreateFileW(
            conin.as_ptr(),
            GENERIC_READ | GENERIC_WRITE,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            std::ptr::null(),
            OPEN_EXISTING,
            0,
            std::ptr::null_mut(),
        );
        let mut written = 0u32;
        let ok = input != INVALID_HANDLE_VALUE
            && WriteConsoleInputW(input, records.as_ptr(), records.len() as u32, &mut written) != 0
            && written == records.len() as u32;
        if input != INVALID_HANDLE_VALUE {
            CloseHandle(input);
        }
        FreeConsole();
        ok
    }
}

#[cfg(not(windows))]
pub fn press(_claude_pid: u32, _key: ConsoleKey) -> bool {
    false
}

/// The prompt glyphs of Claude Code's input box: `❯`, or `>` in consoles
/// that make it fall back to ASCII (seen live in cmd.exe, 2026-09-24).
const PROMPTS: [char; 2] = ['\u{276f}', '>'];
/// How long typed keys get before the screen is read back.
#[cfg(windows)]
const ECHO_WAIT: std::time::Duration = std::time::Duration::from_millis(400);

/// Longest command [`type_line`] types, in characters.
pub const MAX_LINE_CHARS: usize = 200;

/// Whether [`type_line`] may type `text`: one non-blank line of at most
/// [`MAX_LINE_CHARS`] characters without control characters (a newline
/// would submit early, Esc or Backspace would edit the box), line or
/// paragraph separators, or characters outside the Basic Multilingual Plane
/// (one key event per character, so the typed text can be erased again one
/// Backspace per character).
pub fn typable(text: &str) -> bool {
    !text.trim().is_empty()
        && text.chars().count() <= MAX_LINE_CHARS
        && text.chars().all(|c| {
            !c.is_control() && !matches!(c, '\u{2028}' | '\u{2029}') && u32::from(c) <= 0xFFFF
        })
}

/// What [`type_line`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Typed {
    /// The text and Enter went in.
    Sent,
    /// The input box showed something else, a draft of the user's: the typed
    /// text was erased again and nothing was sent (probe TASK-040 P2: a
    /// draft plus `/exit` is submitted as a prompt).
    Draft,
    /// A console step failed or the box was not found; the typed text, if
    /// any, was erased.
    Failed,
}

/// The lines strictly between the last two rule lines (`─` only, 20 or more)
/// of `screen`, blank ones left out: Claude Code's input box. `None`
/// without two rules.
pub fn input_box(screen: &[String]) -> Option<Vec<String>> {
    let rules: Vec<usize> = screen
        .iter()
        .enumerate()
        .filter(|(_, line)| {
            let line = line.trim_end();
            line.chars().count() >= 20 && line.chars().all(|c| c == '\u{2500}')
        })
        .map(|(index, _)| index)
        .collect();
    let [.., top, bottom] = rules[..] else {
        return None;
    };
    Some(
        screen[top + 1..bottom]
            .iter()
            .filter(|line| !line.trim().is_empty())
            .cloned()
            .collect(),
    )
}

/// The box shows exactly the typed `text` and nothing else. Claude Code
/// puts a no-break space after the prompt glyph (probe TASK-040 P2), which
/// `str::trim` removes like any Unicode space. A `!` typed into an empty box
/// switches Claude Code to bash mode, which may show the `!` in place of the
/// glyph: for a text that starts with `!` that form counts too.
pub fn box_shows(lines: &[String], text: &str) -> bool {
    let [line] = lines else {
        return false;
    };
    let text = text.trim();
    if line.strip_prefix(PROMPTS).map(str::trim) == Some(text) {
        return true;
    }
    match (text.strip_prefix('!'), line.strip_prefix('!')) {
        (Some(command), Some(shown)) => shown.trim() == command.trim(),
        _ => false,
    }
}

/// Types `/exit` with [`type_line`].
pub fn type_exit(claude_pid: u32) -> Typed {
    type_line(claude_pid, "/exit")
}

/// Types `text` into the console of `claude_pid`, reads the input box back
/// and presses Enter only when it shows nothing but `text`; otherwise erases
/// the typed characters (Backspace deletes before the cursor, where they
/// went). A text that is not [`typable`] is not typed. Blocking, under a
/// second.
#[cfg(windows)]
pub fn type_line(claude_pid: u32, text: &str) -> Typed {
    use windows_sys::Win32::System::Console::{AttachConsole, FreeConsole, SetConsoleCtrlHandler};

    if !typable(text) {
        return Typed::Failed;
    }
    let _one = one_at_a_time();
    // SAFETY: plain Win32 calls without pointers; see `press`.
    unsafe {
        SetConsoleCtrlHandler(None, 1);
        FreeConsole();
        if AttachConsole(claude_pid) == 0 {
            return Typed::Failed;
        }
    }
    let typed = if write_text(text) {
        std::thread::sleep(ECHO_WAIT);
        let shown = visible_lines()
            .and_then(|screen| input_box(&screen))
            .map(|lines| box_shows(&lines, text));
        if shown == Some(true) && write_text("\r") {
            Typed::Sent
        } else {
            write_text(&"\u{8}".repeat(text.chars().count()));
            match shown {
                Some(false) => Typed::Draft,
                _ => Typed::Failed,
            }
        }
    } else {
        Typed::Failed
    };
    // SAFETY: no arguments.
    unsafe {
        FreeConsole();
    }
    typed
}

#[cfg(not(windows))]
pub fn type_line(_claude_pid: u32, _text: &str) -> Typed {
    Typed::Failed
}

/// One console attachment at a time in this process: [`press`] and
/// [`type_line`] both detach from and attach to a console.
#[cfg(windows)]
fn one_at_a_time() -> std::sync::MutexGuard<'static, ()> {
    static ONE: std::sync::Mutex<()> = std::sync::Mutex::new(());
    ONE.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Writes `text` as key presses (down and up per character; `\r` is Enter,
/// `\u{8}` Backspace) into the input of the console this process is
/// attached to.
#[cfg(windows)]
pub fn write_text(text: &str) -> bool {
    use windows_sys::Win32::Foundation::{
        CloseHandle, GENERIC_READ, GENERIC_WRITE, INVALID_HANDLE_VALUE,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
    };
    use windows_sys::Win32::System::Console::{INPUT_RECORD, KEY_EVENT, WriteConsoleInputW};

    let mut records = Vec::new();
    for unit in text.encode_utf16() {
        let vk = match unit {
            0x0D => 0x0D,
            0x08 => 0x08,
            _ => 0,
        };
        for down in [1, 0] {
            let mut record = INPUT_RECORD {
                EventType: KEY_EVENT as u16,
                ..Default::default()
            };
            // SAFETY: `KeyEvent` is the member `EventType = KEY_EVENT` selects.
            let event = unsafe { &mut record.Event.KeyEvent };
            event.bKeyDown = down;
            event.wRepeatCount = 1;
            event.wVirtualKeyCode = vk;
            event.uChar.UnicodeChar = unit;
            records.push(record);
        }
    }
    let conin: Vec<u16> = "CONIN$\0".encode_utf16().collect();
    // SAFETY: a NUL-terminated wide string, a live record array and its
    // length, a handle closed once.
    unsafe {
        let input = CreateFileW(
            conin.as_ptr(),
            GENERIC_READ | GENERIC_WRITE,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            std::ptr::null(),
            OPEN_EXISTING,
            0,
            std::ptr::null_mut(),
        );
        if input == INVALID_HANDLE_VALUE {
            return false;
        }
        let mut written = 0u32;
        let ok = WriteConsoleInputW(input, records.as_ptr(), records.len() as u32, &mut written)
            != 0
            && written as usize == records.len();
        CloseHandle(input);
        ok
    }
}

#[cfg(not(windows))]
pub fn write_text(_text: &str) -> bool {
    false
}

/// The visible rows of the console this process is attached to, right
/// trimmed. `None` when there is none.
#[cfg(windows)]
pub fn visible_lines() -> Option<Vec<String>> {
    use windows_sys::Win32::Foundation::{
        CloseHandle, GENERIC_READ, GENERIC_WRITE, INVALID_HANDLE_VALUE,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
    };
    use windows_sys::Win32::System::Console::{
        CONSOLE_SCREEN_BUFFER_INFO, COORD, GetConsoleScreenBufferInfo, ReadConsoleOutputCharacterW,
    };

    let conout: Vec<u16> = "CONOUT$\0".encode_utf16().collect();
    // SAFETY: a NUL-terminated wide string; every buffer passed is live and
    // as long as the length given with it; the handle is closed once.
    unsafe {
        let output = CreateFileW(
            conout.as_ptr(),
            GENERIC_READ | GENERIC_WRITE,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            std::ptr::null(),
            OPEN_EXISTING,
            0,
            std::ptr::null_mut(),
        );
        if output == INVALID_HANDLE_VALUE {
            return None;
        }
        let mut info = CONSOLE_SCREEN_BUFFER_INFO::default();
        if GetConsoleScreenBufferInfo(output, &mut info) == 0 {
            CloseHandle(output);
            return None;
        }
        let width = info.dwSize.X.max(0) as usize;
        let mut lines = Vec::new();
        let mut row = vec![0u16; width];
        for y in info.srWindow.Top..=info.srWindow.Bottom {
            let mut read = 0u32;
            let origin = COORD { X: 0, Y: y };
            if ReadConsoleOutputCharacterW(
                output,
                row.as_mut_ptr(),
                width as u32,
                origin,
                &mut read,
            ) == 0
            {
                break;
            }
            let text = String::from_utf16_lossy(&row[..(read as usize).min(width)]);
            lines.push(text.trim_end().to_owned());
        }
        CloseHandle(output);
        Some(lines)
    }
}

#[cfg(not(windows))]
pub fn visible_lines() -> Option<Vec<String>> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_key_is_esc() {
        assert_eq!(key_event(ConsoleKey::Interrupt), (0x1B, 0x01, 0x1B, 0));
    }
    fn screen(lines: &[&str]) -> Vec<String> {
        lines.iter().map(|line| (*line).to_owned()).collect()
    }

    const RULE: &str = "────────────────────────────────────────";

    #[test]
    fn the_input_box_is_between_the_last_two_rules() {
        // Probe TASK-040 P2 (safe_idle): the glyph is followed by U+00A0.
        let idle = screen(&[
            " ▐▛███▛█   Claude Code v2.1.281",
            RULE,
            "old box",
            RULE,
            "❯ earlier prompt",
            "",
            RULE,
            "❯\u{a0}/exit",
            RULE,
            "  ⏵⏵ auto mode on (shift+tab to cycle)",
        ]);
        let found = input_box(&idle).unwrap();
        assert_eq!(found, ["❯\u{a0}/exit"]);
        assert!(box_shows(&found, "/exit"));
        assert!(box_shows(&screen(&["❯ /exit  "]), "/exit"));
        assert!(box_shows(&screen(&["> /cost"]), "/cost"));
        assert!(box_shows(&screen(&[">\u{a0}/exit"]), "/exit"));
        // A draft before or after the cursor (P2 safe_draft), a second
        // line, an empty box, no prompt glyph: no exit.
        for lines in [
            &["❯\u{a0}draft text/exit"][..],
            &["❯\u{a0}/exitdraft"],
            &["❯\u{a0}/exit", "  second line"],
            &["❯"],
            &[],
            &["/exit"],
        ] {
            assert!(!box_shows(&screen(lines), "/exit"), "{lines:?}");
        }
        assert_eq!(input_box(&screen(&[RULE, "❯ x"])), None);
        assert_eq!(
            input_box(&screen(&["───", "❯ x", "───"])),
            None,
            "short rules are not rules"
        );
    }

    #[test]
    fn any_typed_line_is_checked_and_a_bang_may_become_the_prompt() {
        let shows = |lines: &[&str], text: &str| box_shows(&screen(lines), text);
        assert!(shows(
            &["❯\u{a0}/compact keep the plan"],
            "/compact keep the plan"
        ));
        assert!(shows(&["❯\u{a0}!echo hi"], "!echo hi"));
        // Bash mode: the `!` stands where the glyph was.
        assert!(shows(&["!\u{a0}echo hi"], "!echo hi"));
        assert!(shows(&["! echo hi"], "! echo hi"));
        for (lines, text) in [
            (&["❯\u{a0}draft!echo hi"][..], "!echo hi"),
            (&["! draft!echo hi"], "!echo hi"),
            // A box already in bash mode takes the typed `!` as text.
            (&["! !echo hi"], "!echo hi"),
            (&["! echo hi"], "/echo hi"),
            (&["❯\u{a0}/compact", "  more"], "/compact"),
            (&["❯"], "/compact"),
        ] {
            assert!(!shows(lines, text), "{lines:?} {text}");
        }
    }

    #[test]
    fn only_one_short_plain_line_is_typable() {
        assert!(typable("!echo hi"));
        assert!(typable("/model sonnet"));
        assert!(typable("!echo привет"));
        assert!(typable(&"x".repeat(MAX_LINE_CHARS)));
        for text in [
            "",
            "   ",
            "!echo a\nb",
            "!echo a\rb",
            "/x\u{1b}[A",
            "/x\u{8}",
            "/x\ty",
            "/x\u{2028}y",
            "!echo \u{1F600}",
        ] {
            assert!(!typable(text), "{text:?}");
        }
        assert!(!typable(&"x".repeat(MAX_LINE_CHARS + 1)));
    }

    // `press` itself is not called here: it detaches the calling process
    // from its console, which would take the test runner's terminal output
    // with it. The live probe (TASK-029 `scratch/planner/probe`) covers it.
}

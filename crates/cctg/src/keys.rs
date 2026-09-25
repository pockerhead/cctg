//! Presses a key in the console of the agent's Claude Code process
//! (TASK-029): Esc, as when typed in the terminal. Since TASK-040 also types
//! `/exit` for a restart ([`type_exit`], which reads the input box back
//! first) and serves `cctg run` its own console ([`visible_lines`],
//! [`write_text`]); since TASK-043 any one-line command from the topic
//! ([`type_line`], the same safe typing), and closes a panel such a command
//! opened after reading it ([`type_command`]). Since TASK-047 nothing is
//! typed while the screen shows Claude Code's agent view or a running
//! background agent ([`agents_block`]), and a `/exit` that opens Claude
//! Code's "Background work is running" dialog is cancelled with Esc
//! ([`exit_dialog`]).
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

/// How long [`type_command`] watches the screen for a panel after Enter,
/// how often, and how long a found panel gets to fill in before it is read.
#[cfg(windows)]
const PANEL_WAIT: std::time::Duration = std::time::Duration::from_secs(2);
#[cfg(windows)]
const PANEL_POLL: std::time::Duration = std::time::Duration::from_millis(250);
#[cfg(windows)]
const PANEL_SETTLE: std::time::Duration = std::time::Duration::from_millis(700);

/// How long [`type_exit`] watches the screen for [`exit_dialog`] after
/// Enter, and how many Esc it presses at most to close a found one.
#[cfg(windows)]
const EXIT_DIALOG_WAIT: std::time::Duration = std::time::Duration::from_secs(2);
#[cfg(windows)]
const EXIT_DIALOG_ESCAPES: usize = 3;

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
    /// The screen showed the agent view or a running background agent
    /// ([`agents_block`]): nothing was typed. Or `/exit` opened
    /// [`exit_dialog`], which was closed again with Esc: claude stays.
    Agents,
}

/// Whether the screen shows what a typed line would disturb (TASK-047):
/// Claude Code's agent view (its input box offers `Message @<agent>…`, seen
/// live 2026-09-25: typed text goes to that agent, and `/exit` would end the
/// session with its background agents), or the agent list under the status
/// lines with an agent other than `main` marked (`●`, selected or working)
/// or showing a timer (`35m 15s`). Only a list that has a row of `main`
/// alone counts: `●` also starts every tool call line of the conversation.
/// Any other screen does not block (the behaviour before TASK-047).
pub fn agents_block(screen: &[String]) -> bool {
    screen.iter().any(|line| agent_view_prompt(line))
        || agent_list(screen)
            .iter()
            .any(|row| row.name != "main" && (row.marked || row.timer))
}

/// A prompt glyph followed by the agent view's placeholder `Message @<x>`.
fn agent_view_prompt(line: &str) -> bool {
    line.trim_start()
        .strip_prefix(PROMPTS)
        .map(str::trim_start)
        .and_then(|rest| rest.strip_prefix("Message @"))
        .and_then(|name| name.chars().next())
        .is_some_and(|first| !first.is_whitespace())
}

/// One row of Claude Code's agent list: `  ( ) main`,
/// `  ●   maw-qa-medium   Checking… 35m 15s · ↓ 337.7k tokens`, or on the
/// main screen `  ● main` over `  ◯ general-purpose  Append steps  4s · ↓
/// 39.6k tokens` (probe TASK-047, 2.1.282).
#[derive(Debug, PartialEq, Eq)]
struct AgentRow<'a> {
    name: &'a str,
    /// `●`, or a filled radio `(x)` in place of `( )` or `◯`.
    marked: bool,
    /// A duration word (`35m`, `15s`, `1h`) after the name.
    timer: bool,
}

fn agent_row(line: &str) -> Option<AgentRow<'_>> {
    let line = line.trim_start();
    let (marked, rest) = if let Some(rest) = line.strip_prefix("( )") {
        (false, rest)
    } else if let Some(rest) = line.strip_prefix('\u{25ef}') {
        (false, rest)
    } else if let Some(rest) = line.strip_prefix('\u{25cf}') {
        (true, rest)
    } else {
        let mut chars = line.chars();
        match (chars.next(), chars.next(), chars.next()) {
            (Some('('), Some(mark), Some(')')) if !mark.is_whitespace() => (true, chars.as_str()),
            _ => return None,
        }
    };
    if !rest.starts_with(char::is_whitespace) {
        return None;
    }
    let mut words = rest.split_whitespace();
    let name = words.next()?;
    let plain = name
        .chars()
        .all(|c| c.is_alphanumeric() || matches!(c, '-' | '_' | '.' | ':'));
    plain.then(|| AgentRow {
        name,
        marked,
        timer: words.any(duration_word),
    })
}

/// `35m`, `15s`, `1h`, `2m30s`: digits and a unit, once or more.
fn duration_word(word: &str) -> bool {
    let mut digits = false;
    let mut units = false;
    for c in word.chars() {
        if c.is_ascii_digit() {
            digits = true;
        } else if digits && matches!(c, 'h' | 'm' | 's') {
            digits = false;
            units = true;
        } else {
            return false;
        }
    }
    units && !digits
}

/// The agent list on `screen`: the run of agent rows around a row of `main`
/// alone. Empty without one.
fn agent_list(screen: &[String]) -> Vec<AgentRow<'_>> {
    let Some(main) = screen.iter().position(|line| {
        agent_row(line).is_some_and(|row| row.name == "main" && line.trim_end().ends_with("main"))
    }) else {
        return Vec::new();
    };
    let first = screen[..main]
        .iter()
        .rposition(|line| agent_row(line).is_none())
        .map_or(0, |index| index + 1);
    screen[first..]
        .iter()
        .map_while(|line| agent_row(line))
        .collect()
}

/// Whether `screen` shows the dialog Claude Code opens for `/exit` while
/// background work runs (probe TASK-047, 2.1.282): a [`panel`] headed
/// "Background work is running" offering "1. Exit and stop tasks" (selected),
/// "2. Move to background and exit", "3. Stay"; Enter would stop the
/// subagents. Only a panel counts (under the last `▔` edge, no input box
/// after it): the same words in the conversation above do not.
pub fn exit_dialog(screen: &[String]) -> bool {
    let Some(top) = screen.iter().rposition(|line| panel_edge(line)) else {
        return false;
    };
    let below = &screen[top..];
    input_box(below).is_none()
        && below
            .iter()
            .any(|line| line.trim() == "Background work is running")
}

/// The top edge of a [`panel`]: 20 or more `▔` at the start of the line.
fn panel_edge(line: &str) -> bool {
    line.trim_start()
        .chars()
        .take_while(|&c| c == '\u{2594}')
        .count()
        >= 20
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

/// The text of the panel on `screen`: what `/cost` or `/usage` open in
/// place of the input box (probe 2026-09-24, 2.1.282), under a top edge of
/// `▔`. `None` while an input box is on screen or without that edge. Lines
/// lose their common indent, runs of blank lines and the `↓` scroll mark.
pub fn panel(screen: &[String]) -> Option<String> {
    if input_box(screen).is_some() {
        return None;
    }
    let top = screen.iter().rposition(|line| panel_edge(line))?;
    let lines: Vec<&str> = screen[top + 1..]
        .iter()
        .map(|line| line.trim_end())
        .filter(|line| line.trim() != "\u{2193}")
        .collect();
    let indent = lines
        .iter()
        .filter(|line| !line.is_empty())
        .map(|line| line.bytes().take_while(|&b| b == b' ').count())
        .min()
        .unwrap_or(0);
    let mut text = String::new();
    for line in lines {
        if line.is_empty() {
            if !text.is_empty() && !text.ends_with("\n\n") {
                text.push('\n');
            }
        } else {
            text.push_str(&line[indent..]);
            text.push('\n');
        }
    }
    Some(text.trim_end().to_owned())
}

/// Types `/exit` with [`type_line`]. When it opens [`exit_dialog`], the
/// dialog is closed with Esc (never an option picked) and the answer is
/// [`Typed::Agents`]; [`Typed::Failed`] when it stays open.
pub fn type_exit(claude_pid: u32) -> Typed {
    type_and_watch(claude_pid, "/exit", After::ExitDialog).0
}

/// What [`type_and_watch`] looks for once the line went in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum After {
    Nothing,
    Panel,
    ExitDialog,
}

/// Types `text` into the console of `claude_pid`, reads the input box back
/// and presses Enter only when it shows nothing but `text`; otherwise erases
/// the typed characters (Backspace deletes before the cursor, where they
/// went). A text that is not [`typable`] is not typed. Blocking, under a
/// second.
pub fn type_line(claude_pid: u32, text: &str) -> Typed {
    type_and_watch(claude_pid, text, After::Nothing).0
}

/// [`type_line`], then, once sent, watches the screen for a [`panel`]; a
/// panel that shows up is read and closed with Esc (it waits for Esc and
/// blocks the terminal otherwise). Blocking, up to about three seconds.
pub fn type_command(claude_pid: u32, text: &str) -> (Typed, Option<String>) {
    type_and_watch(claude_pid, text, After::Panel)
}

#[cfg(windows)]
fn type_and_watch(claude_pid: u32, text: &str, after: After) -> (Typed, Option<String>) {
    use windows_sys::Win32::System::Console::{AttachConsole, FreeConsole, SetConsoleCtrlHandler};

    if !typable(text) {
        return (Typed::Failed, None);
    }
    let _one = one_at_a_time();
    // SAFETY: plain Win32 calls without pointers; see `press`.
    unsafe {
        SetConsoleCtrlHandler(None, 1);
        FreeConsole();
        if AttachConsole(claude_pid) == 0 {
            return (Typed::Failed, None);
        }
    }
    let agents = visible_lines().is_some_and(|screen| agents_block(&screen));
    let typed = if agents {
        Typed::Agents
    } else if write_text(text) {
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
    let (typed, panel) = match (typed, after) {
        (Typed::Sent, After::Panel) => (typed, close_panel()),
        (Typed::Sent, After::ExitDialog) => (cancel_exit_dialog(), None),
        _ => (typed, None),
    };
    // SAFETY: no arguments.
    unsafe {
        FreeConsole();
    }
    (typed, panel)
}

#[cfg(not(windows))]
fn type_and_watch(_claude_pid: u32, _text: &str, _after: After) -> (Typed, Option<String>) {
    (Typed::Failed, None)
}

/// Reads the console of `claude_pid` and says whether it shows
/// [`agents_block`]; `false` when it cannot be read. Blocking and short.
#[cfg(windows)]
pub fn agents_on_screen(claude_pid: u32) -> bool {
    use windows_sys::Win32::System::Console::{AttachConsole, FreeConsole, SetConsoleCtrlHandler};

    let _one = one_at_a_time();
    // SAFETY: plain Win32 calls without pointers; see `press`.
    unsafe {
        SetConsoleCtrlHandler(None, 1);
        FreeConsole();
        if AttachConsole(claude_pid) == 0 {
            return false;
        }
    }
    let agents = visible_lines().is_some_and(|screen| agents_block(&screen));
    // SAFETY: no arguments.
    unsafe {
        FreeConsole();
    }
    agents
}

#[cfg(not(windows))]
pub fn agents_on_screen(_claude_pid: u32) -> bool {
    false
}

/// Waits up to [`PANEL_WAIT`] for a [`panel`] in the attached console; reads
/// a found one after [`PANEL_SETTLE`] and presses Esc. Esc goes only to a
/// panel on screen: without one it would interrupt a turn.
#[cfg(windows)]
fn close_panel() -> Option<String> {
    let until = std::time::Instant::now() + PANEL_WAIT;
    while std::time::Instant::now() < until {
        std::thread::sleep(PANEL_POLL);
        if visible_lines().as_deref().and_then(panel).is_some() {
            std::thread::sleep(PANEL_SETTLE);
            let text = visible_lines().as_deref().and_then(panel);
            if text.is_some() {
                write_text("\u{1b}");
            }
            return text;
        }
    }
    None
}

/// Watches the attached console up to [`EXIT_DIALOG_WAIT`] after `/exit`
/// for [`exit_dialog`]. Without one claude is exiting: [`Typed::Sent`]. A
/// found one gets Esc (cancel, claude stays), again while it is still shown,
/// at most [`EXIT_DIALOG_ESCAPES`] times: [`Typed::Agents`] once it is
/// gone, [`Typed::Failed`] when it stays.
#[cfg(windows)]
fn cancel_exit_dialog() -> Typed {
    let shown = || visible_lines().is_some_and(|screen| exit_dialog(&screen));
    let until = std::time::Instant::now() + EXIT_DIALOG_WAIT;
    loop {
        std::thread::sleep(PANEL_POLL);
        if shown() {
            break;
        }
        if std::time::Instant::now() >= until {
            return Typed::Sent;
        }
    }
    for _ in 0..EXIT_DIALOG_ESCAPES {
        write_text("\u{1b}");
        std::thread::sleep(ECHO_WAIT);
        if !shown() {
            return Typed::Agents;
        }
    }
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
/// `\u{8}` Backspace, `\u{1b}` Esc) into the input of the console this
/// process is attached to.
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
            0x1B => 0x1B,
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
    fn a_panel_is_read_only_without_an_input_box() {
        // Probe 2026-09-24 (2.1.282): `/cost` open, then after Esc.
        let top = format!(
            "{} \u{25d0} medium \u{b7} /effort \u{2594}",
            "\u{2594}".repeat(40)
        );
        let open = screen(&[
            " \u{2590}\u{259b}\u{2588}\u{2588}\u{2588}\u{259b}\u{2588}   Claude Code v2.1.282",
            "",
            &top,
            "   Settings  Status   Config   Usage   Stats",
            "",
            "   Session",
            "   Total cost:            $0.0000",
            "",
            "",
            "   Current session",
            "   \u{2588}\u{2588}\u{2588}\u{258c}          7% used",
            "",
            "                                    \u{2193}",
        ]);
        assert_eq!(
            panel(&open).as_deref(),
            Some(
                "Settings  Status   Config   Usage   Stats\n\nSession\nTotal cost:            \
                 $0.0000\n\nCurrent session\n\u{2588}\u{2588}\u{2588}\u{258c}          7% used"
            )
        );
        // Closed: the box is back. `/context` prints above the box: no panel.
        let closed = screen(&[&top, "   Session", RULE, "\u{276f}", RULE]);
        assert_eq!(panel(&closed), None);
        assert_eq!(panel(&screen(&["   Session", "   Total cost: $0"])), None);
    }

    /// The live case of TASK-047 (2026-09-25), without the user's text: the
    /// view of subagent `maw-qa-medium` open, two background agents working.
    fn agent_view() -> Vec<String> {
        screen(&[
            " \u{2590}\u{259b}\u{2588}\u{2588}\u{2588}\u{259c}\u{258c}   Claude Code v2.1.282",
            "",
            "\u{25cf} Agent(QA pass)",
            "  \u{23bf}  Backgrounded agent",
            "",
            ">\u{a0}Message @maw-qa-medium\u{2026}",
            "  \u{23f5}\u{23f5} auto mode on (shift+tab to cycle) \u{b7} \u{2190} 2 agents",
            "",
            "  ( ) main",
            "  \u{25cf}   maw-qa-medium               Checking git stat\u{2026} 35m 15s \u{b7} \u{2193} 337.7k tokens",
            "  ( ) maw-plan-reviewer-2-medium  Reading hold_fire\u{2026}  6m 10s \u{b7} \u{2193} 189.0k tokens",
        ])
    }

    #[test]
    fn the_agent_view_and_working_background_agents_block_typing() {
        let live = agent_view();
        assert!(agents_block(&live));
        // Each sign alone blocks: the placeholder, a marked agent, a timer.
        let placeholder = &live[..8];
        assert!(agents_block(placeholder));
        let mut list: Vec<String> = live.clone();
        list[5] = ">\u{a0}".into();
        assert!(agents_block(&list));
        list[9] = "  ( ) maw-qa-medium".into();
        assert!(agents_block(&list), "the timer of the other agent");
        list[10] = "  ( ) maw-plan-reviewer-2-medium".into();
        assert!(!agents_block(&list), "a list of idle agents");
        for line in ["❯\u{a0}Message @general-purpose\u{2026}", "  > Message @x"] {
            assert!(agents_block(&screen(&[line])), "{line}");
        }
        for row in ["  (\u{2022}) worker", "  \u{25cf} worker"] {
            assert!(
                agents_block(&screen(&["  ( ) main", row])),
                "a marked agent: {row}"
            );
        }
        assert!(agents_block(&screen(&[
            "  \u{25cf} main",
            "  ( ) worker   Running 2m30s",
        ])));
    }

    #[test]
    fn a_plain_screen_does_not_block_typing() {
        // A finished background agent: only the count in the status line.
        let idle = screen(&[
            "\u{25cf} Agent(QA pass)",
            "  \u{23bf}  Done (12 tool uses \u{b7} 40.1k tokens \u{b7} 3m 2s)",
            "",
            "\u{25cf} Bash(sleep 5)",
            "  \u{23bf}  Running\u{2026} (5s)",
            "\u{25cf} main is up to date 5s ago",
            "",
            RULE,
            "\u{276f}\u{a0}",
            RULE,
            "  \u{23f5}\u{23f5} auto mode on (shift+tab to cycle) \u{b7} \u{2190} 1 agent",
        ]);
        assert!(!agents_block(&idle));
        for lines in [
            &["\u{276f}\u{a0}/exit"][..],
            &["\u{276f}\u{a0}Message me when done"],
            &["\u{276f}\u{a0}Message @"],
            &["Message @x"],
            // Main alone selected, the others idle.
            &["  \u{25cf} main", "  ( ) worker"],
            // Rows without a list of main.
            &["  \u{25cf} worker  12s", "  ( ) other  3m"],
            &["  ( ) main  and more", "  \u{25cf} worker"],
            &[],
        ] {
            assert!(!agents_block(&screen(lines)), "{lines:?}");
        }
        for word in ["35m", "15s", "1h", "2m30s"] {
            assert!(duration_word(word), "{word}");
        }
        for word in ["m", "5", "5x", "337.7k", "s5", "5m3"] {
            assert!(!duration_word(word), "{word}");
        }
    }

    /// The main screen of the TASK-047 probe (2.1.282) while a background
    /// subagent works, the prompt left out: the list's rows are `● main`
    /// and `◯ <type>  <description>  <time> · ↓ <tokens>`.
    fn probe_main_screen(agent_row: &str) -> Vec<String> {
        screen(&[
            "\u{25cf} Agent(Append steps with sleeps)",
            "  \u{23bf}  Backgrounded agent (\u{2193} to manage \u{b7} ctrl+o to expand)",
            "\u{25cf} launched",
            "\u{273b} Waiting for 1 background agent to finish",
            RULE,
            "\u{276f}",
            RULE,
            "  Opus 5.5 [medium] dir:cctg-t047-probe sh:bash ctx:6%",
            "  \u{23f5}\u{23f5} bypass permissions on (shift+tab to cycle) \u{b7} \u{2190} 1 agent",
            "  \u{25cf} main",
            agent_row,
        ])
    }

    #[test]
    fn a_working_agent_on_the_main_screen_blocks_typing() {
        for row in [
            // Right after the launch, and after a resume by SendMessage.
            "  \u{25ef} general-purpose  Append steps with sleeps          4s \u{b7} \u{2193} 39.6k tokens",
            "  \u{25ef} general-purpose  Append steps with sleeps          1s",
        ] {
            assert!(agents_block(&probe_main_screen(row)), "{row}");
        }
        assert!(
            !agents_block(&probe_main_screen(
                "  \u{25ef} general-purpose  Append steps with sleeps"
            )),
            "an agent without a timer"
        );
    }

    /// The dialog `/exit` opened in the TASK-047 probe (2.1.282) while a
    /// background subagent worked.
    fn probe_exit_dialog() -> Vec<String> {
        screen(&[
            "\u{25cf} Agent(Append steps with sleeps)",
            "  \u{23bf}  Backgrounded agent (\u{2193} to manage \u{b7} ctrl+o to expand)",
            "\u{25cf} launched",
            "\u{273b} Waiting for 1 background agent to finish",
            &"\u{2594}".repeat(120),
            "   Background work is running",
            "   The following will stop when you exit:",
            "   subagent \u{b7} Append steps with sleeps",
            "   \u{276f} 1. Exit and stop tasks",
            "     2. Move to background and exit",
            "     3. Stay",
            "   Enter to confirm \u{b7} Esc to cancel",
        ])
    }

    #[test]
    fn the_exit_dialog_is_seen_only_as_a_panel() {
        assert!(exit_dialog(&probe_exit_dialog()));
        // Rules of an earlier box above the dialog do not hide it.
        let mut earlier = screen(&[RULE, "\u{276f} earlier", RULE]);
        earlier.extend(probe_exit_dialog());
        assert!(exit_dialog(&earlier));
        // Closed with Esc: the input box is back.
        assert!(!exit_dialog(&probe_main_screen(
            "  \u{25ef} general-purpose  x  4s"
        )));
        // The words in the conversation, above an input box.
        let quoted = screen(&[
            &"\u{2594}".repeat(40),
            "   Background work is running",
            RULE,
            "\u{276f}",
            RULE,
        ]);
        assert!(!exit_dialog(&quoted));
        assert!(!exit_dialog(&screen(&["   Background work is running"])));
        // Another panel (`/cost`).
        let cost = screen(&[&"\u{2594}".repeat(40), "   Session", "   Total cost: $0"]);
        assert!(!exit_dialog(&cost));
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

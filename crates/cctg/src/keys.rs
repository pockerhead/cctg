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
//! ([`exit_dialog`]). Since TASK-057 faint text in the input box (Claude
//! Code's prompt suggestion, placeholder, inline completion) is not taken
//! for a draft where the screen read tells it ([`typed_box`]), and a typed
//! line wrapped over several rows of the box is joined back ([`box_shows`]).
//!
//! Channels have no command for it, so on Windows the agent (a child of
//! that claude) attaches to its console and writes the key events into the
//! console input: `FreeConsole` + `AttachConsole(claude pid)` + `CONIN$` +
//! `WriteConsoleInputW`, then `FreeConsole` again. A console is attached per
//! process, so one press at a time. The agent's stdio are pipes and stay
//! untouched. On Linux and macOS (TASK-044) the agent asks the `cctg run`
//! that started its claude, which keeps claude in a pseudo-terminal
//! ([`crate::term`]), for its screen copy and to write into claude's input.
//! Both ways are a [`Target`]; what to type and when is decided on the same
//! screen lines for both.
//!
//! A successful write says only that the key events are in the console
//! input buffer (behind any input that waits there), not that Claude Code
//! read them or that its turn ended.

use std::path::PathBuf;
use std::time::Duration;

use crate::wire::ConsoleKey;

/// Where the agent presses and types.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// The console of this claude process (Windows only).
    Console(u32),
    /// The terminal of the `cctg run` behind this socket (Unix only).
    Run(PathBuf),
}

/// `(virtual key, scan code, character, control key state)` of `key`.
pub fn key_event(key: ConsoleKey) -> (u16, u16, u16, u32) {
    match key {
        ConsoleKey::Interrupt => (0x1B, 0x01, 0x1B, 0),
    }
}

/// Writes `key` (down and up) into `target`. Blocking and short; `false`
/// when any step failed.
pub fn press(target: &Target, key: ConsoleKey) -> bool {
    match (target, key) {
        (Target::Console(pid), key) => press_console(*pid, key),
        (Target::Run(socket), ConsoleKey::Interrupt) => crate::term::keys(socket, "\u{1b}"),
    }
}

#[cfg(windows)]
fn press_console(claude_pid: u32, key: ConsoleKey) -> bool {
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
fn press_console(_claude_pid: u32, _key: ConsoleKey) -> bool {
    false
}

/// The prompt glyphs of Claude Code's input box: `❯`, or `>` in consoles
/// that make it fall back to ASCII (seen live in cmd.exe, 2026-09-24).
const PROMPTS: [char; 2] = ['\u{276f}', '>'];

/// How long typing waits for the screen: typed keys before the box is read
/// back; after Enter, a [`panel`] ([`type_command`]) and [`exit_dialog`]
/// ([`type_exit`]), how often they are looked for, how long a found panel
/// gets to fill in, and Esc presses to close the dialog at most.
#[derive(Debug, Clone, Copy)]
struct Waits {
    echo: Duration,
    panel: Duration,
    poll: Duration,
    settle: Duration,
    exit_dialog: Duration,
    exit_escapes: usize,
}

const WAITS: Waits = Waits {
    echo: Duration::from_millis(400),
    panel: Duration::from_secs(2),
    poll: Duration::from_millis(250),
    settle: Duration::from_millis(700),
    exit_dialog: Duration::from_secs(2),
    exit_escapes: 3,
};

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

/// Whether the screen shows Claude Code's agent view (TASK-047): its input
/// box offers `Message @<agent>…` (seen live 2026-09-25), and a typed line
/// would go to that agent. Background agents on the main screen do not block
/// here: `/exit` opens Claude Code's own "Background work is running" dialog,
/// which [`type_exit`] cancels, and the hub holds updates while subagents
/// run; the agent list's markers show the selected view, not a running
/// agent, so they are not read.
pub fn agents_block(screen: &[String]) -> bool {
    // Only the input box: an earlier prompt "Message @…" in the history
    // above must not block (QA TASK-047).
    input_box(screen).is_some_and(|lines| lines.iter().any(|line| agent_view_prompt(line)))
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
    box_rows(screen).map(|rows| non_blank(&screen[rows]))
}

/// [`input_box`] of what was typed: faint text (placeholder, prompt
/// suggestion, inline completion) left out where the screen read can tell
/// ([`crate::term::Rows`]), as if the box showed only its [`input_box`]
/// otherwise.
pub fn typed_box(rows: &crate::term::Rows) -> Option<Vec<String>> {
    let found = box_rows(&rows.lines)?;
    match &rows.solid {
        Some(solid) if solid.len() == rows.lines.len() => Some(non_blank(&solid[found])),
        _ => Some(non_blank(&rows.lines[found])),
    }
}

/// The row indexes strictly between the last two rule lines of `screen`.
fn box_rows(screen: &[String]) -> Option<std::ops::Range<usize>> {
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
    Some(top + 1..bottom)
}

fn non_blank(lines: &[String]) -> Vec<String> {
    lines
        .iter()
        .filter(|line| !line.trim().is_empty())
        .cloned()
        .collect()
}

/// The box shows exactly the typed `text` and nothing else. Claude Code
/// puts a no-break space after the prompt glyph (probe TASK-040 P2), which
/// `str::trim` removes like any Unicode space. A `!` typed into an empty box
/// switches Claude Code to bash mode, which may show the `!` in place of the
/// glyph: for a text that starts with `!` that form counts too. A text too
/// long for one row goes on in the next rows of the box, indented, broken
/// after a word (the space dropped) or, without one, inside it (probe
/// TASK-057): the rows are joined back.
pub fn box_shows(lines: &[String], text: &str) -> bool {
    let Some((line, rest)) = lines.split_first() else {
        return false;
    };
    let text = text.trim();
    let joined = |first: &str, text: &str| {
        let pieces = std::iter::once(first.trim()).chain(rest.iter().map(|line| line.trim()));
        !first.trim().is_empty() && joins(pieces, text)
    };
    if line
        .strip_prefix(PROMPTS)
        .is_some_and(|shown| joined(shown, text))
    {
        return true;
    }
    match (text.strip_prefix('!'), line.strip_prefix('!')) {
        (Some(command), Some(shown)) => joined(shown, command.trim()),
        _ => false,
    }
}

/// `text` is `pieces` one after another, with any whitespace between two.
fn joins<'a>(pieces: impl Iterator<Item = &'a str>, text: &str) -> bool {
    let mut rest = text;
    for (index, piece) in pieces.enumerate() {
        if index > 0 {
            rest = rest.trim_start();
        }
        let Some(after) = rest.strip_prefix(piece) else {
            return false;
        };
        rest = after;
    }
    rest.is_empty()
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
pub fn type_exit(target: &Target) -> Typed {
    type_and_watch(target, "/exit", After::ExitDialog).0
}

/// What [`type_and_watch`] looks for once the line went in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum After {
    Nothing,
    Panel,
    ExitDialog,
}

/// Types `text` into `target`, reads the input box back and presses Enter
/// only when it shows nothing but `text`; otherwise erases the typed
/// characters (Backspace deletes before the cursor, where they went). A text
/// that is not [`typable`] is not typed. Blocking, under a second.
pub fn type_line(target: &Target, text: &str) -> Typed {
    type_and_watch(target, text, After::Nothing).0
}

/// [`type_line`], then, once sent, watches the screen for a [`panel`]; a
/// panel that shows up is read and closed with Esc (it waits for Esc and
/// blocks the terminal otherwise). Blocking, up to about three seconds.
pub fn type_command(target: &Target, text: &str) -> (Typed, Option<String>) {
    type_and_watch(target, text, After::Panel)
}

/// Reads `target` and says whether it shows [`agents_block`]; `false` when
/// it cannot be read. Blocking and short.
pub fn agents_on_screen(target: &Target) -> bool {
    open(target)
        .and_then(|mut terminal| terminal.lines())
        .is_some_and(|screen| agents_block(&screen))
}

/// A claude terminal as typing needs it.
trait Terminal {
    /// The visible rows, right trimmed, with their faint text where known.
    fn rows(&mut self) -> Option<crate::term::Rows>;
    /// The visible rows, right trimmed.
    fn lines(&mut self) -> Option<Vec<String>> {
        self.rows().map(|rows| rows.lines)
    }
    /// Writes `text` as key presses: `\r` is Enter, `\u{8}` Backspace,
    /// `\u{1b}` Esc.
    fn write(&mut self, text: &str) -> bool;
}

fn open(target: &Target) -> Option<Box<dyn Terminal>> {
    match target {
        Target::Console(pid) => console(*pid),
        Target::Run(socket) => Some(Box::new(Run(socket.clone()))),
    }
}

fn type_and_watch(target: &Target, text: &str, after: After) -> (Typed, Option<String>) {
    if !typable(text) {
        return (Typed::Failed, None);
    }
    match open(target) {
        Some(mut terminal) => watch(terminal.as_mut(), text, after, WAITS),
        None => (Typed::Failed, None),
    }
}

/// The typing of [`type_and_watch`] on an open terminal.
fn watch(
    terminal: &mut dyn Terminal,
    text: &str,
    after: After,
    waits: Waits,
) -> (Typed, Option<String>) {
    let agents = terminal.lines().is_some_and(|screen| agents_block(&screen));
    let typed = if agents {
        Typed::Agents
    } else if terminal.write(text) {
        std::thread::sleep(waits.echo);
        let shown = terminal
            .rows()
            .and_then(|rows| typed_box(&rows))
            .map(|lines| box_shows(&lines, text));
        if shown == Some(true) && terminal.write("\r") {
            Typed::Sent
        } else {
            terminal.write(&"\u{8}".repeat(text.chars().count()));
            match shown {
                Some(false) => Typed::Draft,
                _ => Typed::Failed,
            }
        }
    } else {
        Typed::Failed
    };
    match (typed, after) {
        (Typed::Sent, After::Panel) => (typed, close_panel(terminal, waits)),
        (Typed::Sent, After::ExitDialog) => (cancel_exit_dialog(terminal, waits), None),
        _ => (typed, None),
    }
}

/// Waits up to `waits.panel` for a [`panel`]; reads a found one after
/// `waits.settle` and presses Esc. Esc goes only to a panel on screen:
/// without one it would interrupt a turn.
fn close_panel(terminal: &mut dyn Terminal, waits: Waits) -> Option<String> {
    let until = std::time::Instant::now() + waits.panel;
    while std::time::Instant::now() < until {
        std::thread::sleep(waits.poll);
        if terminal.lines().as_deref().and_then(panel).is_some() {
            std::thread::sleep(waits.settle);
            let text = terminal.lines().as_deref().and_then(panel);
            if text.is_some() {
                terminal.write("\u{1b}");
            }
            return text;
        }
    }
    None
}

/// Watches the terminal up to `waits.exit_dialog` after `/exit` for
/// [`exit_dialog`]. Without one claude is exiting: [`Typed::Sent`]. A found
/// one gets Esc (cancel, claude stays), again while it is still shown, at
/// most `waits.exit_escapes` times: [`Typed::Agents`] once it is gone,
/// [`Typed::Failed`] when it stays.
fn cancel_exit_dialog(terminal: &mut dyn Terminal, waits: Waits) -> Typed {
    let mut shown = || terminal.lines().is_some_and(|screen| exit_dialog(&screen));
    let until = std::time::Instant::now() + waits.exit_dialog;
    loop {
        std::thread::sleep(waits.poll);
        if shown() {
            break;
        }
        if std::time::Instant::now() >= until {
            return Typed::Sent;
        }
    }
    for _ in 0..waits.exit_escapes {
        terminal.write("\u{1b}");
        std::thread::sleep(waits.echo);
        if !terminal.lines().is_some_and(|screen| exit_dialog(&screen)) {
            return Typed::Agents;
        }
    }
    Typed::Failed
}

/// The terminal of a `cctg run` (Linux, macOS): every read and write is one
/// ask on its socket.
struct Run(PathBuf);

impl Terminal for Run {
    fn rows(&mut self) -> Option<crate::term::Rows> {
        crate::term::rows(&self.0)
    }

    /// A terminal sends DEL for Backspace. A text goes in as its first
    /// character, then the rest: like typing, a leading `!` or `/` reaches
    /// Claude Code alone (a `!` switches the empty box to bash mode).
    fn write(&mut self, text: &str) -> bool {
        let text = text.replace('\u{8}', "\u{7f}");
        let mut chars = text.chars();
        let (Some(first), rest) = (chars.next(), chars.as_str()) else {
            return true;
        };
        if first.is_control() || rest.is_empty() {
            return crate::term::keys(&self.0, &text);
        }
        crate::term::keys(&self.0, first.encode_utf8(&mut [0; 4])) && {
            std::thread::sleep(Duration::from_millis(50));
            crate::term::keys(&self.0, rest)
        }
    }
}

/// This process attached to a claude console (Windows): detached again and
/// the lock released when dropped.
#[cfg(windows)]
struct Attached {
    _one: std::sync::MutexGuard<'static, ()>,
}

#[cfg(windows)]
fn attach(claude_pid: u32) -> Option<Attached> {
    use windows_sys::Win32::System::Console::{AttachConsole, FreeConsole, SetConsoleCtrlHandler};

    let one = one_at_a_time();
    // SAFETY: plain Win32 calls without pointers; see `press_console`.
    unsafe {
        SetConsoleCtrlHandler(None, 1);
        FreeConsole();
        if AttachConsole(claude_pid) == 0 {
            return None;
        }
    }
    Some(Attached { _one: one })
}

#[cfg(windows)]
fn console(claude_pid: u32) -> Option<Box<dyn Terminal>> {
    attach(claude_pid).map(|console| Box::new(console) as Box<dyn Terminal>)
}

#[cfg(not(windows))]
fn console(_claude_pid: u32) -> Option<Box<dyn Terminal>> {
    None
}

#[cfg(windows)]
impl Drop for Attached {
    fn drop(&mut self) {
        // SAFETY: no arguments.
        unsafe {
            windows_sys::Win32::System::Console::FreeConsole();
        }
    }
}

#[cfg(windows)]
impl Terminal for Attached {
    /// No faint text: the console's attribute words do not carry it (probe
    /// TASK-057: Claude Code's faint placeholder reads as 0x0007, like typed
    /// text).
    fn rows(&mut self) -> Option<crate::term::Rows> {
        visible_lines().map(|lines| crate::term::Rows { lines, solid: None })
    }

    fn write(&mut self, text: &str) -> bool {
        write_text(text)
    }
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
            RULE,
            ">\u{a0}Message @maw-qa-medium\u{2026}",
            RULE,
            "  \u{23f5}\u{23f5} auto mode on (shift+tab to cycle) \u{b7} \u{2190} 2 agents",
            "",
            "  ( ) main",
            "  \u{25cf}   maw-qa-medium               Checking git stat\u{2026} 35m 15s \u{b7} \u{2193} 337.7k tokens",
            "  ( ) maw-plan-reviewer-2-medium  Reading hold_fire\u{2026}  6m 10s \u{b7} \u{2193} 189.0k tokens",
        ])
    }

    #[test]
    fn only_the_agent_view_blocks_typing() {
        let live = agent_view();
        assert!(agents_block(&live));
        for line in ["❯\u{a0}Message @general-purpose\u{2026}", "  > Message @x"] {
            assert!(agents_block(&screen(&[RULE, line, RULE])), "{line}");
        }
        // The agent list alone (the view closed) does not block: /exit is
        // guarded by Claude Code's own dialog and the hub.
        let mut list: Vec<String> = live.clone();
        list[6] = ">\u{a0}".into();
        assert!(!agents_block(&list));
        // An earlier prompt in the history does not block, the box does.
        let history = screen(&[
            "\u{276f} Message @bob about the build",
            RULE,
            "\u{276f}\u{a0}",
            RULE,
        ]);
        assert!(!agents_block(&history));
    }

    #[test]
    fn a_plain_screen_does_not_block_typing() {
        let idle = screen(&[
            "\u{25cf} Agent(QA pass)",
            "  \u{23bf}  Done (12 tool uses \u{b7} 40.1k tokens \u{b7} 3m 2s)",
            "",
            RULE,
            "\u{276f}\u{a0}",
            RULE,
            "  \u{23f5}\u{23f5} auto mode on (shift+tab to cycle) \u{b7} \u{2190} 1 agent",
            "  \u{25cf} main",
            "  \u{25ef} general-purpose  Append steps with sleeps          4s \u{b7} \u{2193} 39.6k tokens",
        ]);
        assert!(!agents_block(&idle));
        for lines in [
            &["\u{276f}\u{a0}/exit"][..],
            &["\u{276f}\u{a0}Message me when done"],
            &["\u{276f}\u{a0}Message @"],
            &["Message @x"],
            &[],
        ] {
            assert!(!agents_block(&screen(lines)), "{lines:?}");
        }
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
        assert!(!exit_dialog(&screen(&[
            "\u{25cf} launched",
            RULE,
            "\u{276f}",
            RULE,
            "  \u{25cf} main",
            "  \u{25ef} general-purpose  x  4s",
        ])));
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

    /// A claude stand-in for [`watch`], the typing both platforms share: an
    /// input box that echoes what is typed after the user's `draft`
    /// (Backspace erases, the typed text first), Enter submits; `/cost`
    /// opens a panel, `/exit` opens the background-work dialog while `busy`,
    /// Esc closes either (unless `stuck`). `view`: the agent view is open.
    /// `ghost`: faint text Claude Code shows after the input (a prompt
    /// suggestion in an empty box, an inline completion after typed text),
    /// told apart only when `faint` (a Unix screen, not a Windows console).
    #[derive(Default)]
    struct Fake {
        draft: String,
        input: String,
        ghost: String,
        faint: bool,
        panel: bool,
        dialog: bool,
        busy: bool,
        stuck: bool,
        view: bool,
        sent: Vec<String>,
        escapes: usize,
    }

    impl Fake {
        /// The screen, with the ghost when `ghost` is set.
        fn screen(&self, ghost: bool) -> Vec<String> {
            let mut lines = vec!["\u{25cf} earlier answer".to_owned()];
            if self.panel {
                lines.push("\u{2594}".repeat(40));
                lines.push("   Session".to_owned());
                lines.push("   Total cost: $0".to_owned());
            } else if self.dialog {
                lines.extend(probe_exit_dialog());
            } else {
                let shown = format!("{}{}", self.draft, self.input);
                let shown = if self.view && shown.is_empty() {
                    "Message @qa\u{2026}".to_owned()
                } else {
                    shown
                };
                let ghost = if ghost { self.ghost.as_str() } else { "" };
                lines.push(RULE.to_owned());
                lines.push(format!("\u{276f}\u{a0}{shown}{ghost}"));
                lines.push(RULE.to_owned());
            }
            lines
        }
    }

    impl Terminal for Fake {
        fn rows(&mut self) -> Option<crate::term::Rows> {
            Some(crate::term::Rows {
                lines: self.screen(true),
                solid: self.faint.then(|| self.screen(false)),
            })
        }

        fn write(&mut self, text: &str) -> bool {
            for c in text.chars() {
                match c {
                    '\r' => {
                        let line = format!("{}{}", self.draft, self.input);
                        self.draft.clear();
                        self.input.clear();
                        self.panel = line == "/cost";
                        self.dialog = line == "/exit" && self.busy;
                        self.sent.push(line);
                    }
                    '\u{8}' => {
                        if self.input.pop().is_none() {
                            self.draft.pop();
                        }
                    }
                    '\u{1b}' => {
                        self.escapes += 1;
                        if !self.stuck {
                            self.panel = false;
                            self.dialog = false;
                        }
                    }
                    c => self.input.push(c),
                }
            }
            true
        }
    }

    const QUICK: Waits = Waits {
        echo: Duration::from_millis(1),
        panel: Duration::from_millis(50),
        poll: Duration::from_millis(1),
        settle: Duration::from_millis(1),
        exit_dialog: Duration::from_millis(50),
        exit_escapes: 3,
    };

    #[test]
    fn a_line_goes_in_only_into_an_empty_box() {
        let mut claude = Fake::default();
        let typed = watch(&mut claude, "/model sonnet", After::Nothing, QUICK);
        assert_eq!(typed, (Typed::Sent, None));
        assert_eq!(claude.sent, ["/model sonnet"]);
        // A draft of the user's: the typed text is erased, the draft stays.
        let mut claude = Fake {
            draft: "fix the".into(),
            ..Fake::default()
        };
        assert_eq!(
            watch(&mut claude, "/exit", After::ExitDialog, QUICK),
            (Typed::Draft, None)
        );
        assert!(claude.sent.is_empty());
        assert_eq!(
            (claude.draft.as_str(), claude.input.as_str()),
            ("fix the", "")
        );
        // The agent view: nothing is typed at all.
        let mut claude = Fake {
            view: true,
            ..Fake::default()
        };
        assert_eq!(
            watch(&mut claude, "!ls", After::Nothing, QUICK),
            (Typed::Agents, None)
        );
        assert!(claude.sent.is_empty() && claude.input.is_empty());
    }

    #[test]
    fn faint_text_in_the_box_is_not_a_draft() {
        // The live case of TASK-057 (Linux, 2026-09-26): a prompt suggestion
        // in the empty box, or a completion after the typed text.
        let mut claude = Fake {
            ghost: "\u{0414}\u{0430}, \u{0434}\u{0430}\u{0432}\u{0430}\u{0439} T2I".into(),
            faint: true,
            ..Fake::default()
        };
        assert_eq!(
            watch(&mut claude, "!curl -s localhost", After::Nothing, QUICK),
            (Typed::Sent, None)
        );
        assert_eq!(claude.sent, ["!curl -s localhost"]);
        // A draft of the user's still blocks, ghost or not.
        let mut claude = Fake {
            draft: "fix the".into(),
            ghost: " tests".into(),
            faint: true,
            ..Fake::default()
        };
        assert_eq!(
            watch(&mut claude, "/exit", After::ExitDialog, QUICK),
            (Typed::Draft, None)
        );
        assert!(claude.sent.is_empty() && claude.input.is_empty());
        // A screen read that cannot tell faint text (a Windows console, a
        // `cctg run` older than the `rows` ask) keeps refusing, as before.
        let mut claude = Fake {
            ghost: " -la".into(),
            ..Fake::default()
        };
        assert_eq!(
            watch(&mut claude, "!ls", After::Nothing, QUICK),
            (Typed::Draft, None)
        );
        assert!(claude.sent.is_empty() && claude.input.is_empty());
    }

    #[test]
    fn the_typed_box_leaves_out_faint_text_only_where_known() {
        let rows = |solid: Option<&[&str]>| crate::term::Rows {
            lines: screen(&[RULE, "\u{276f}\u{a0}!ls -la", "  more", RULE]),
            solid: solid.map(screen),
        };
        assert_eq!(
            typed_box(&rows(Some(&[RULE, "\u{276f}\u{a0}!ls", "", RULE]))),
            Some(screen(&["\u{276f}\u{a0}!ls"]))
        );
        let whole = Some(screen(&["\u{276f}\u{a0}!ls -la", "  more"]));
        assert_eq!(typed_box(&rows(None)), whole);
        // Solid rows that do not match the screen are not used.
        assert_eq!(typed_box(&rows(Some(&["\u{276f}\u{a0}!ls"]))), whole);
        assert_eq!(
            typed_box(&crate::term::Rows {
                lines: screen(&["\u{276f} x"]),
                solid: None
            }),
            None
        );
    }

    #[test]
    fn a_line_wrapped_in_the_box_is_joined_back() {
        // Probe TASK-057 (2.1.283, 120 columns): a bash command broken after
        // a word, a line without spaces broken inside it.
        let words = (0..25)
            .map(|i| format!("word{i:02}"))
            .collect::<Vec<_>>()
            .join(" ");
        let (head, tail) = words.split_at(words.find(" word16").unwrap());
        let command = format!("!echo {words}");
        let shown = [format!("!\u{a0}echo {head}"), format!(" {tail}")];
        assert!(box_shows(&shown, &command));
        let glyph = [format!("\u{276f}\u{a0}echo {head}"), format!(" {tail}")];
        assert!(box_shows(&glyph, &format!("echo {words}")));
        let xs = "x".repeat(150);
        let hard = [
            format!("\u{276f}\u{a0}{}", &xs[..118]),
            format!("  {}", &xs[118..]),
        ];
        assert!(box_shows(&hard, &xs));
        // Anything more or less than the typed text still counts as a draft:
        // a draft line after or before it, a missing piece, a glyph alone.
        for (lines, text) in [
            (&["\u{276f}\u{a0}/exit", "  draft"][..], "/exit"),
            (&["\u{276f}\u{a0}draft", "  /exit"], "/exit"),
            (&["\u{276f}", "  /exit"], "/exit"),
            (&["\u{276f}\u{a0}!echo a", "  b"], "!echo a b c"),
            (&["\u{276f}\u{a0}!echo a", "  bc"], "!echo a b"),
        ] {
            assert!(!box_shows(&screen(lines), text), "{lines:?} {text}");
        }
    }

    #[test]
    fn a_panel_is_read_and_closed_and_esc_goes_only_to_a_panel() {
        let mut claude = Fake::default();
        assert_eq!(
            watch(&mut claude, "/cost", After::Panel, QUICK),
            (Typed::Sent, Some("Session\nTotal cost: $0".to_owned()))
        );
        assert_eq!((claude.panel, claude.escapes), (false, 1));
        let mut claude = Fake::default();
        assert_eq!(
            watch(&mut claude, "/compact", After::Panel, QUICK),
            (Typed::Sent, None)
        );
        assert_eq!(
            claude.escapes, 0,
            "no Esc without a panel: it would interrupt"
        );
    }

    #[test]
    fn the_background_work_dialog_of_exit_is_cancelled() {
        let mut claude = Fake::default();
        assert_eq!(
            watch(&mut claude, "/exit", After::ExitDialog, QUICK),
            (Typed::Sent, None)
        );
        assert_eq!(claude.escapes, 0);
        let mut claude = Fake {
            busy: true,
            ..Fake::default()
        };
        assert_eq!(
            watch(&mut claude, "/exit", After::ExitDialog, QUICK),
            (Typed::Agents, None)
        );
        assert_eq!((claude.dialog, claude.escapes), (false, 1));
        let mut claude = Fake {
            busy: true,
            stuck: true,
            ..Fake::default()
        };
        assert_eq!(
            watch(&mut claude, "/exit", After::ExitDialog, QUICK),
            (Typed::Failed, None)
        );
        assert_eq!(claude.escapes, 3);
    }

    #[test]
    fn a_run_terminal_that_does_not_answer_gets_nothing() {
        let target = Target::Run(std::env::temp_dir().join("cctg-no-such-run.sock"));
        assert_eq!(type_line(&target, "/cost"), Typed::Failed);
        assert!(!agents_on_screen(&target));
        assert!(!press(&target, ConsoleKey::Interrupt));
    }

    // A console `press` is not called here: it detaches the calling process
    // from its console, which would take the test runner's terminal output
    // with it. The live probe (TASK-029 `scratch/planner/probe`) covers it.
}

//! Files between the topic and the session (TASK-032): the parts both ends
//! of the agent link share.
//!
//! A file crosses the link as [`CHUNK`]-byte pieces in base64 `file_chunk`
//! lines ([`chunks`]); the receiving end puts them together with an
//! [`Assembly`], which takes only the next piece in order and never more
//! than the announced size. A sender keeps at most [`AHEAD`] lines waiting
//! in a link queue ([`room`]), so other messages never wait behind a whole
//! file and a big file is never all encoded at once.
//!
//! The agent keeps a file from the topic with [`save`]: in
//! `<session folder>/.cctg/inbox/` (Claude reads it there without asking; a
//! `.gitignore` of its own keeps the inbox out of `git status`), else in
//! the device's own `<state>/inbox/<session>/` (owner-only; not the temp
//! dir, where other sessions' sandboxed commands could read it, TASK-087),
//! as `<UTC date>-<name>` with the name cleaned by
//! [`clean_name`], and never over an existing file. `send_file` reads with
//! [`read_upload`].
//!
//! Nothing here logs: names, captions and contents are the user's.

use std::fs::OpenOptions;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use tokio::sync::mpsc;

use crate::wire::{FileChunk, FileKind};

/// Raw bytes per chunk a sender makes: a line of about 90 KB, written well
/// inside the 5 s a link write may take even on a slow remote link.
pub const CHUNK: usize = 64 << 10;
/// Largest chunk a receiver takes (a line of about 350 KB, inside
/// `wire::MAX_LINE`): more than [`CHUNK`], so the two ends need not agree.
pub const MAX_CHUNK: usize = 256 << 10;
/// Lines a sender lets wait in a link queue.
pub const AHEAD: usize = 4;
/// Largest file a bot may download (`getFile`; `20 << 20` in telegram-bot-api).
pub const MAX_DOWNLOAD: u64 = 20 << 20;
/// Largest file a bot may upload (`sendDocument`, "50 MB").
pub const MAX_UPLOAD: u64 = 50 << 20;
/// Largest photo (`sendPhoto`, "10 MB"); a bigger picture goes as a document.
pub const MAX_PHOTO: u64 = 10 << 20;
/// Longest cleaned file name, in bytes.
pub const MAX_NAME: usize = 120;
/// Tries of `-2`, `-3`, ... before a save gives up.
const MAX_COPIES: u32 = 1000;
/// How often [`room`] looks again.
const ROOM_POLL: Duration = Duration::from_millis(5);

/// The `file_chunk`s of `bytes`, in order.
pub fn chunks(transfer_id: u64, bytes: &[u8]) -> impl Iterator<Item = FileChunk> + '_ {
    bytes
        .chunks(CHUNK)
        .enumerate()
        .map(move |(index, piece)| FileChunk {
            transfer_id,
            offset: (index * CHUNK) as u64,
            data: STANDARD.encode(piece),
        })
}

/// Why a transfer was dropped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum Broken {
    #[error("file piece out of order")]
    Order,
    #[error("file piece is not base64")]
    Encoding,
    #[error("file piece past the announced size")]
    Size,
}

/// A file being received: the bytes so far, in order, up to its size.
#[derive(Debug)]
pub struct Assembly {
    size: u64,
    bytes: Vec<u8>,
}

impl Assembly {
    /// The announced size is not trusted for the allocation.
    pub fn new(size: u64) -> Self {
        let first = usize::try_from(size).unwrap_or(usize::MAX).min(4 * CHUNK);
        Self {
            size,
            bytes: Vec::with_capacity(first),
        }
    }

    pub fn size(&self) -> u64 {
        self.size
    }

    /// Takes the next piece; `Ok(true)` once the file is complete.
    pub fn push(&mut self, chunk: &FileChunk) -> Result<bool, Broken> {
        if chunk.offset != self.bytes.len() as u64 {
            return Err(Broken::Order);
        }
        if chunk.data.len() > base64::encoded_len(MAX_CHUNK, true).unwrap_or(usize::MAX) {
            return Err(Broken::Size);
        }
        let piece = STANDARD.decode(&chunk.data).map_err(|_| Broken::Encoding)?;
        if piece.is_empty() {
            return Err(Broken::Order);
        }
        if self.bytes.len() as u64 + piece.len() as u64 > self.size {
            return Err(Broken::Size);
        }
        self.bytes.extend_from_slice(&piece);
        Ok(self.is_complete())
    }

    pub fn is_complete(&self) -> bool {
        self.bytes.len() as u64 == self.size
    }

    pub fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }
}

/// Waits until fewer than [`AHEAD`] messages wait in `queue`; `false` when
/// it closed.
pub async fn room<T>(queue: &mpsc::Sender<T>) -> bool {
    loop {
        if queue.is_closed() {
            return false;
        }
        if queue.max_capacity() - queue.capacity() < AHEAD {
            return true;
        }
        tokio::time::sleep(ROOM_POLL).await;
    }
}

/// JPEG, PNG or WebP by their first bytes, the pictures `sendPhoto` takes.
/// The name is not asked.
pub fn is_photo(bytes: &[u8]) -> bool {
    bytes.starts_with(&[0xFF, 0xD8, 0xFF])
        || bytes.starts_with(b"\x89PNG\r\n\x1a\n")
        || (bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP")
}

/// The name of a file Telegram gave no name: its kind and the extension of
/// its Telegram path (`photos/file_3.jpg` -> `photo.jpg`).
pub fn default_name(kind: FileKind, telegram_path: Option<&str>) -> String {
    let extension = telegram_path
        .and_then(|path| Path::new(path).extension())
        .and_then(|extension| extension.to_str())
        .filter(|extension| {
            extension.len() <= 8 && extension.bytes().all(|b| b.is_ascii_alphanumeric())
        });
    match extension {
        Some(extension) => format!("{}.{extension}", kind.as_str()),
        None => kind.as_str().to_owned(),
    }
}

/// `raw` made safe as one file name on any system: only its last path
/// part; the characters Windows forbids, control and bidi characters as
/// `_`; no dots or spaces at either end (no hidden files, no `..`); a
/// reserved device name (`NUL.txt`, `com1`) gets a leading `_`; at most
/// [`MAX_NAME`] bytes, keeping a short extension. Empty: `fallback`.
pub fn clean_name(raw: &str, fallback: &str) -> String {
    let last = raw.rsplit(['/', '\\']).next().unwrap_or_default();
    let replaced: String = last
        .chars()
        .map(|c| if forbidden(c) { '_' } else { c })
        .collect();
    let trimmed = trim_ends(&replaced);
    let mut name = if trimmed.is_empty() {
        fallback.to_owned()
    } else {
        trimmed.to_owned()
    };
    if reserved(&name) {
        name.insert(0, '_');
    }
    shorten(name)
}

fn forbidden(c: char) -> bool {
    c.is_control()
        || matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*')
        || matches!(c, '\u{200E}' | '\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}')
}

fn trim_ends(name: &str) -> &str {
    name.trim_matches(|c: char| c == '.' || c.is_whitespace())
}

/// `CON`, `PRN`, `AUX`, `NUL`, `COM0`-`COM9`, `LPT0`-`LPT9` (also with
/// ¹²³), with any extension: Windows opens the device instead.
fn reserved(name: &str) -> bool {
    let stem = name
        .split('.')
        .next()
        .unwrap_or_default()
        .trim_end()
        .to_uppercase();
    if matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL") {
        return true;
    }
    let mut chars = stem.chars();
    let head: String = chars.by_ref().take(3).collect();
    let tail: Vec<char> = chars.collect();
    (head == "COM" || head == "LPT")
        && matches!(tail.as_slice(), [digit] if digit.is_ascii_digit() || matches!(digit, '¹' | '²' | '³'))
}

fn shorten(name: String) -> String {
    if name.len() <= MAX_NAME {
        return name;
    }
    let extension = name
        .rfind('.')
        .filter(|&dot| dot > 0 && name.len() - dot <= 16)
        .map_or("", |dot| &name[dot..]);
    let room = MAX_NAME - extension.len();
    let stem = trim_ends(&name[..name.floor_char_boundary(room)]);
    format!("{stem}{extension}")
}

/// `name` with `-n` before its extension (`n` > 1).
fn numbered(name: &str, n: u32) -> String {
    if n == 1 {
        return name.to_owned();
    }
    match name.rfind('.').filter(|&dot| dot > 0) {
        Some(dot) => format!("{}-{n}{}", &name[..dot], &name[dot..]),
        None => format!("{name}-{n}"),
    }
}

/// `YYYY-MM-DD` of `now` in UTC.
pub fn date(now: SystemTime) -> String {
    let days = now
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs() / 86_400);
    // Days to a civil date (H. Hinnant, "chrono-Compatible Low-Level Date
    // Algorithms"), for days since 1970 only.
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}")
}

/// Where files from the topic go, in order of preference: the session
/// folder's inbox, then `fallback` ([`fallback_inbox`]).
pub fn inboxes(work: Option<&Path>, fallback: Option<&Path>) -> Vec<PathBuf> {
    work.map(|work| work.join(".cctg").join("inbox"))
        .into_iter()
        .chain(fallback.map(Path::to_path_buf))
        .collect()
}

/// The inbox for when the session folder cannot hold a file:
/// `<state>/inbox/<session>` in the device state dir (`~/.cctg` unless
/// `CCTG_STATE_DIR`), which sandboxed commands cannot read (TASK-087 review
/// round 2: in the temp dir they could). A session id that is no plain name
/// becomes `session`.
pub fn fallback_inbox(state: &Path, session: Option<&str>) -> PathBuf {
    let session = session
        .filter(|id| crate::update::is_session_id(id))
        .unwrap_or("session");
    state.join("inbox").join(session)
}

/// Saves `bytes` as a new file `<date>-<name>` (`name` already cleaned) in
/// the first of [`inboxes`] that takes it; returns its path. A taken name
/// gets `-2`, `-3`, ...; nothing is ever overwritten.
///
/// `sandboxed` (TASK-087): only `<work>/.cctg/inbox`, never `fallback`
/// (the model could not reach it), and only when neither `.cctg` nor the
/// inbox is a link or a file and the inbox resolves inside `work` (a
/// sandboxed command could have pointed it anywhere). A swap between that
/// check and the write remains possible (docs/sandbox.md).
pub fn save(
    work: Option<&Path>,
    fallback: Option<&Path>,
    name: &str,
    bytes: &[u8],
    now: SystemTime,
    sandboxed: bool,
) -> io::Result<PathBuf> {
    let dated = format!("{}-{name}", date(now));
    if sandboxed {
        let work = work.ok_or_else(|| io::Error::other("no session folder"))?;
        return save_in(&confined_inbox(work)?, &dated, bytes, false);
    }
    let mut last = io::Error::other("no inbox");
    for dir in inboxes(work, fallback) {
        let private = fallback.is_some_and(|fallback| dir == fallback);
        match save_in(&dir, &dated, bytes, private) {
            Ok(path) => return Ok(path),
            Err(error) => last = error,
        }
    }
    Err(last)
}

/// `<work>/.cctg/inbox`, made one folder at a time, with no link on the way
/// and resolving inside `work`.
fn confined_inbox(work: &Path) -> io::Result<PathBuf> {
    let cctg = work.join(".cctg");
    let inbox = cctg.join("inbox");
    for dir in [&cctg, &inbox] {
        match std::fs::symlink_metadata(dir) {
            Ok(meta) if meta.file_type().is_dir() => {}
            Ok(_) => return Err(io::Error::other("the inbox is not a plain folder")),
            Err(error) if error.kind() == io::ErrorKind::NotFound => std::fs::create_dir(dir)?,
            Err(error) => return Err(error),
        }
    }
    let inside = match (
        crate::sandbox::paths::canonical(work),
        crate::sandbox::paths::canonical(&inbox),
    ) {
        (Some(work), Some(real)) => crate::sandbox::paths::within(&work, &real),
        _ => false,
    };
    if inside {
        Ok(inbox)
    } else {
        Err(io::Error::other("the inbox is outside the session folder"))
    }
}

/// `private`: the fallback inbox of the device state dir, made 0700 with
/// files 0600 on unix, like the hook spool.
fn save_in(dir: &Path, name: &str, bytes: &[u8], private: bool) -> io::Result<PathBuf> {
    if private {
        crate::sandbox::create_private_dir(dir)?;
    } else {
        std::fs::create_dir_all(dir)?;
        keep_out_of_git(dir);
    }
    for n in 1..=MAX_COPIES {
        let path = dir.join(numbered(name, n));
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        if private {
            std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        }
        match options.open(&path) {
            Ok(mut file) => {
                if let Err(error) = file.write_all(bytes).and_then(|()| file.flush()) {
                    drop(file);
                    let _ = std::fs::remove_file(&path);
                    return Err(error);
                }
                return Ok(path);
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::from(io::ErrorKind::AlreadyExists))
}

/// A `.gitignore` of `*` in the inbox: git skips the inbox and nothing
/// else; the user's own `.gitignore` files stay as they are. An existing
/// one is left alone; a failure only means the inbox shows in `git status`.
fn keep_out_of_git(inbox: &Path) {
    let _ = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(inbox.join(".gitignore"))
        .and_then(|mut file| file.write_all(b"*\n"));
}

/// Why `send_file` did not take a path. The texts are for Claude.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum UploadError {
    #[error("`path` is not a regular file on this machine")]
    NotFile,
    #[error("the file is empty; Telegram takes no empty files")]
    Empty,
    #[error("the file is larger than 50 MB, the Telegram limit for bots")]
    TooBig,
    #[error("the file cannot be read ({0:?})")]
    Io(io::ErrorKind),
}

/// Reads a file for `send_file`: a regular file (a link to one counts) of
/// 1 to [`MAX_UPLOAD`] bytes, as it is while read. A Windows device path is
/// refused before it is opened: a named pipe passes `is_file` and its read
/// never ends (QA TASK-032).
pub fn read_upload(path: &Path) -> Result<Vec<u8>, UploadError> {
    let io = |error: io::Error| UploadError::Io(error.kind());
    if device_path(path) {
        return Err(UploadError::NotFile);
    }
    let metadata = std::fs::metadata(path).map_err(|_| UploadError::NotFile)?;
    if !metadata.is_file() {
        return Err(UploadError::NotFile);
    }
    if metadata.len() > MAX_UPLOAD {
        return Err(UploadError::TooBig);
    }
    read_upload_from(std::fs::File::open(path).map_err(io)?)
}

/// [`read_upload`] of a file already open: the checks go by the handle, so
/// what is read is what was checked (TASK-087: a sandboxed command may swap
/// the path for a link at any moment).
pub fn read_upload_from(file: std::fs::File) -> Result<Vec<u8>, UploadError> {
    let io = |error: io::Error| UploadError::Io(error.kind());
    let metadata = file.metadata().map_err(io)?;
    if !metadata.is_file() {
        return Err(UploadError::NotFile);
    }
    if metadata.len() > MAX_UPLOAD {
        return Err(UploadError::TooBig);
    }
    let mut bytes = Vec::new();
    file.take(MAX_UPLOAD + 1)
        .read_to_end(&mut bytes)
        .map_err(io)?;
    match bytes.len() as u64 {
        0 => Err(UploadError::Empty),
        len if len > MAX_UPLOAD => Err(UploadError::TooBig),
        _ => Ok(bytes),
    }
}

/// A path in the Windows device namespace: `\\.\...` (pipes, consoles,
/// devices), or `\\?\...` that is neither a drive (`\\?\C:\`) nor a share
/// (`\\?\UNC\`). Either slash counts.
fn device_path(path: &Path) -> bool {
    let text = path.to_string_lossy().replace('/', r"\");
    if text.starts_with(r"\\.\") {
        return true;
    }
    let Some(rest) = text.strip_prefix(r"\\?\") else {
        return false;
    };
    let first = rest.split('\\').next().unwrap_or("");
    let drive = first.len() == 2 && first.ends_with(':');
    !(drive || first.eq_ignore_ascii_case("UNC"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hub::testdir::TempDir;

    #[test]
    fn device_paths_are_refused_before_they_are_opened() {
        for path in [
            r"\\.\pipe\cctg-test",
            "//./pipe/cctg-test",
            r"\\?\pipe\cctg-test",
            r"\\.\CONIN$",
            r"\\?\GLOBALROOT\Device\Null",
        ] {
            assert!(device_path(Path::new(path)), "{path}");
            assert_eq!(read_upload(Path::new(path)), Err(UploadError::NotFile));
        }
        for path in [
            r"C:\x\a.png",
            r"\\?\C:\x\a.png",
            r"\\?\UNC\host\share\a.png",
            "a.png",
        ] {
            assert!(!device_path(Path::new(path)), "{path}");
        }
    }

    #[test]
    fn chunks_put_together_give_the_file_and_fit_a_link_line() {
        let bytes: Vec<u8> = (0..(2 * CHUNK + 17)).map(|n| (n % 251) as u8).collect();
        let pieces: Vec<FileChunk> = chunks(9, &bytes).collect();
        assert_eq!(pieces.len(), 3);
        assert_eq!(
            pieces.iter().map(|piece| piece.offset).collect::<Vec<_>>(),
            [0, CHUNK as u64, 2 * CHUNK as u64]
        );
        let line = crate::wire::encode(&crate::wire::AgentMsg::FileChunk(pieces[0].clone()));
        assert!(line.len() < crate::wire::MAX_LINE / 2, "{}", line.len());
        let mut assembly = Assembly::new(bytes.len() as u64);
        assert_eq!(assembly.push(&pieces[0]), Ok(false));
        assert_eq!(assembly.push(&pieces[1]), Ok(false));
        assert_eq!(assembly.push(&pieces[2]), Ok(true));
        assert_eq!(assembly.into_bytes(), bytes);
        assert_eq!(chunks(1, &[]).count(), 0);
        assert!(Assembly::new(0).is_complete());
    }

    #[test]
    fn an_assembly_takes_only_the_next_piece_and_never_more_than_its_size() {
        let bytes = vec![7u8; CHUNK + 5];
        let pieces: Vec<FileChunk> = chunks(1, &bytes).collect();
        // Out of order, repeated, or past the size: broken.
        let mut assembly = Assembly::new(bytes.len() as u64);
        assert_eq!(assembly.push(&pieces[1]), Err(Broken::Order));
        assert_eq!(assembly.push(&pieces[0]), Ok(false));
        assert_eq!(assembly.push(&pieces[0]), Err(Broken::Order));
        let mut small = Assembly::new(3);
        assert_eq!(small.push(&pieces[0]), Err(Broken::Size));
        let bad = FileChunk {
            transfer_id: 1,
            offset: 0,
            data: "not base64!".into(),
        };
        assert_eq!(Assembly::new(9).push(&bad), Err(Broken::Encoding));
        let empty = FileChunk {
            data: String::new(),
            ..bad.clone()
        };
        assert_eq!(Assembly::new(9).push(&empty), Err(Broken::Order));
        // A peer's piece up to MAX_CHUNK is taken, even when bigger than ours.
        let peer = FileChunk {
            data: STANDARD.encode(vec![1u8; MAX_CHUNK]),
            ..bad.clone()
        };
        let line = crate::wire::encode(&crate::wire::HubMsg::FileChunk(peer.clone()));
        assert!(line.len() < crate::wire::MAX_LINE / 2, "{}", line.len());
        assert_eq!(Assembly::new(MAX_CHUNK as u64).push(&peer), Ok(true));
        let long = FileChunk {
            data: "A".repeat(base64::encoded_len(MAX_CHUNK, true).unwrap() + 4),
            ..bad
        };
        assert_eq!(Assembly::new(u64::MAX).push(&long), Err(Broken::Size));
        // A huge announced size allocates nothing like it.
        assert!(Assembly::new(u64::MAX).bytes.capacity() <= 4 * CHUNK);
    }

    #[tokio::test]
    async fn room_waits_while_the_queue_holds_its_share() {
        let (tx, mut rx) = mpsc::channel::<u8>(16);
        for n in 0..AHEAD as u8 {
            tx.send(n).await.unwrap();
        }
        let waiting = tokio::spawn({
            let tx = tx.clone();
            async move { room(&tx).await }
        });
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(!waiting.is_finished(), "a full share waits");
        rx.recv().await.unwrap();
        assert!(waiting.await.unwrap());
        drop(rx);
        assert!(!room(&tx).await, "a closed queue has no room");
    }

    #[test]
    fn photos_are_known_by_their_bytes_not_their_names() {
        assert!(is_photo(&[0xFF, 0xD8, 0xFF, 0xE0, 0, 0]));
        assert!(is_photo(b"\x89PNG\r\n\x1a\n\0\0"));
        assert!(is_photo(b"RIFF\0\0\0\0WEBPVP8 "));
        for other in [
            &b"GIF89a"[..],
            b"%PDF-1.7",
            b"RIFF\0\0\0\0WAVE",
            b"",
            b"\xFF\xD8",
        ] {
            assert!(!is_photo(other), "{other:?}");
        }
    }

    #[test]
    fn names_are_cleaned_for_every_system() {
        for (raw, want) in [
            ("report.pdf", "report.pdf"),
            ("../../etc/passwd", "passwd"),
            (r"C:\Users\x\evil.exe", "evil.exe"),
            ("a<b>c:d\"e|f?g*h.txt", "a_b_c_d_e_f_g_h.txt"),
            ("tab\there\u{7}.txt", "tab_here_.txt"),
            ("..", "file"),
            ("  .hidden. ", "hidden"),
            ("", "file"),
            ("NUL.txt", "_NUL.txt"),
            ("con", "_con"),
            ("Com1.tar.gz", "_Com1.tar.gz"),
            ("lpt\u{b9}", "_lpt\u{b9}"),
            ("COM10.txt", "COM10.txt"),
            ("console.log", "console.log"),
            ("\u{202E}fdp.exe", "_fdp.exe"),
            ("Скриншот 2026.png", "Скриншот 2026.png"),
        ] {
            assert_eq!(clean_name(raw, "file"), want, "{raw:?}");
        }
        let long = format!("{}.jpeg", "я".repeat(200));
        let cut = clean_name(&long, "file");
        assert!(cut.len() <= MAX_NAME && cut.ends_with("я.jpeg"), "{cut}");
        let no_extension = "x".repeat(300);
        assert_eq!(clean_name(&no_extension, "file").len(), MAX_NAME);
        assert_eq!(numbered("a.b.png", 2), "a.b-2.png");
        assert_eq!(numbered(".profile", 3), ".profile-3");
        assert_eq!(numbered("notes", 1), "notes");
    }

    #[test]
    fn a_nameless_file_is_named_by_its_kind_and_telegram_path() {
        assert_eq!(
            default_name(FileKind::Photo, Some("photos/file_3.jpg")),
            "photo.jpg"
        );
        assert_eq!(
            default_name(FileKind::Voice, Some("voice/file_1.oga")),
            "voice.oga"
        );
        assert_eq!(default_name(FileKind::Video, None), "video");
        assert_eq!(
            default_name(FileKind::Audio, Some("music/x.m/p3?")),
            "audio"
        );
    }

    #[test]
    fn dates_are_utc_calendar_days() {
        let at = |secs: u64| date(UNIX_EPOCH + Duration::from_secs(secs));
        assert_eq!(at(0), "1970-01-01");
        assert_eq!(at(951_782_400), "2000-02-29");
        assert_eq!(at(1_709_251_199), "2024-02-29");
        assert_eq!(at(1_790_294_399), "2026-09-24");
        assert_eq!(at(1_790_294_400), "2026-09-25");
    }

    #[test]
    fn a_save_never_overwrites_and_keeps_the_inbox_out_of_git() {
        let dir = TempDir::new("files-save");
        let now = UNIX_EPOCH + Duration::from_secs(1_790_294_400);
        let first = save(Some(dir.path()), None, "shot.png", b"one", now, false).unwrap();
        let second = save(Some(dir.path()), None, "shot.png", b"two", now, false).unwrap();
        let inbox = dir.path().join(".cctg").join("inbox");
        assert_eq!(first, inbox.join("2026-09-25-shot.png"));
        assert_eq!(second, inbox.join("2026-09-25-shot-2.png"));
        assert_eq!(std::fs::read(&first).unwrap(), b"one");
        assert_eq!(std::fs::read(&second).unwrap(), b"two");
        let ignore = inbox.join(".gitignore");
        assert_eq!(std::fs::read_to_string(&ignore).unwrap(), "*\n");
        // An existing .gitignore is the user's: left as it is.
        std::fs::write(&ignore, "mine\n").unwrap();
        save(Some(dir.path()), None, "shot.png", b"three", now, false).unwrap();
        assert_eq!(std::fs::read_to_string(&ignore).unwrap(), "mine\n");
        // A folder that cannot hold the inbox falls back to the device's
        // own inbox, never the temp dir (TASK-087).
        let blocked = dir.path().join("blocked");
        std::fs::write(&blocked, b"a file, not a folder").unwrap();
        let own = fallback_inbox(&dir.path().join("state"), Some("5e55-ab"));
        assert_eq!(own, dir.path().join("state").join("inbox").join("5e55-ab"));
        let fallback = save(Some(&blocked), Some(&own), "x.txt", b"four", now, false).unwrap();
        assert_eq!(fallback, own.join("2026-09-25-x.txt"));
        assert!(!own.join(".gitignore").exists(), "not in a repository");
        assert!(save(Some(&blocked), None, "x.txt", b"five", now, false).is_err());
        assert_eq!(
            fallback_inbox(Path::new("s"), Some("../x")),
            Path::new("s").join("inbox").join("session")
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = |path: &Path| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode(&own), 0o700);
            assert_eq!(mode(&fallback), 0o600);
        }
    }

    #[test]
    fn uploads_are_regular_files_of_a_size_telegram_takes() {
        let dir = TempDir::new("files-upload");
        let file = dir.path().join("a.bin");
        std::fs::write(&file, b"abc").unwrap();
        assert_eq!(read_upload(&file), Ok(b"abc".to_vec()));
        assert_eq!(read_upload(dir.path()), Err(UploadError::NotFile));
        assert_eq!(
            read_upload(&dir.path().join("missing")),
            Err(UploadError::NotFile)
        );
        let empty = dir.path().join("empty");
        std::fs::write(&empty, b"").unwrap();
        assert_eq!(read_upload(&empty), Err(UploadError::Empty));
        let big = dir.path().join("big");
        std::fs::File::create(&big)
            .unwrap()
            .set_len(MAX_UPLOAD + 1)
            .unwrap();
        assert_eq!(read_upload(&big), Err(UploadError::TooBig));
        let exact = dir.path().join("exact");
        std::fs::File::create(&exact)
            .unwrap()
            .set_len(MAX_UPLOAD)
            .unwrap();
        assert_eq!(
            read_upload(&exact).map(|bytes| bytes.len() as u64),
            Ok(MAX_UPLOAD)
        );
    }

    /// TASK-087: a sandboxed session keeps files in its folder only, never
    /// in the temp folder and never through a link.
    #[test]
    fn a_sandboxed_save_stays_in_the_folder() {
        let dir = TempDir::new("files-sandboxed");
        let now = UNIX_EPOCH + Duration::from_secs(1_790_294_400);
        let work = dir.path().join("proj");
        std::fs::create_dir_all(&work).unwrap();
        let own = dir.path().join("state-inbox");
        let saved = save(Some(&work), Some(&own), "a.txt", b"one", now, true).unwrap();
        assert_eq!(
            saved,
            work.join(".cctg").join("inbox").join("2026-09-25-a.txt")
        );
        assert_eq!(
            inboxes(Some(&work), None),
            [work.join(".cctg").join("inbox")]
        );
        assert_eq!(inboxes(Some(&work), Some(&own)).len(), 2);
        assert!(save(None, Some(&own), "a.txt", b"one", now, true).is_err());
        // A file where the inbox should be: no fallback anywhere.
        let blocked = dir.path().join("blocked");
        std::fs::create_dir_all(blocked.join(".cctg")).unwrap();
        std::fs::write(blocked.join(".cctg").join("inbox"), b"x").unwrap();
        assert!(save(Some(&blocked), Some(&own), "x.txt", b"two", now, true).is_err());
        assert!(!own.exists(), "a sandboxed session never falls back");
        #[cfg(unix)]
        {
            let outside = dir.path().join("outside");
            std::fs::create_dir_all(&outside).unwrap();
            let linked = dir.path().join("linked");
            std::fs::create_dir_all(linked.join(".cctg")).unwrap();
            std::os::unix::fs::symlink(&outside, linked.join(".cctg").join("inbox")).unwrap();
            assert!(save(Some(&linked), Some(&own), "x.txt", b"three", now, true).is_err());
            let whole = dir.path().join("whole");
            std::fs::create_dir_all(&whole).unwrap();
            std::os::unix::fs::symlink(&outside, whole.join(".cctg")).unwrap();
            assert!(save(Some(&whole), Some(&own), "x.txt", b"four", now, true).is_err());
            assert_eq!(std::fs::read_dir(&outside).unwrap().count(), 0);
        }
    }

    #[test]
    fn an_open_file_is_read_by_its_handle() {
        let dir = TempDir::new("files-handle");
        let file = dir.path().join("a.bin");
        std::fs::write(&file, b"abc").unwrap();
        let open = std::fs::File::open(&file).unwrap();
        assert_eq!(read_upload_from(open), Ok(b"abc".to_vec()));
        let folder = std::fs::File::open(dir.path());
        if let Ok(folder) = folder {
            assert_eq!(read_upload_from(folder), Err(UploadError::NotFile));
        }
    }

    #[test]
    fn the_inbox_ignores_only_itself() {
        let dir = TempDir::new("files-ignore");
        let now = UNIX_EPOCH + Duration::from_secs(1_790_294_400);
        save(Some(dir.path()), None, "shot.png", b"one", now, false).unwrap();
        let cctg = dir.path().join(".cctg");
        assert_eq!(
            std::fs::read_to_string(cctg.join("inbox").join(".gitignore")).unwrap(),
            "*\n"
        );
        // Other things a project keeps in `.cctg` stay visible to git.
        assert!(!cctg.join(".gitignore").exists());
    }
}

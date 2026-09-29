//! Voice messages to text on the hub (TASK-085).
//!
//! The slots actor sends a [`Job`] per voice message of a topic; one task
//! downloads the file (at most [`MAX_VOICE_BYTES`]) and runs the helper
//! `cctg-voice` on it, one voice at a time, and gets back one [`Heard`] per
//! job. The helper is a process of its own per voice: it decodes the
//! OGG/Opus and recognizes it with the model baked into the hub image, then
//! exits, so its memory goes back to the system. It gets the bytes on stdin
//! and answers one JSON line; it never inherits the bot token, the hub
//! secret or a proxy (see [`scrubbed`]).
//!
//! Logs carry lengths, times and outcomes, never the words, the caption,
//! the model path or the bytes.

use std::ffi::OsStr;
use std::future::Future;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use serde::Deserialize;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::mpsc;
use tokio::time::Instant;
use tracing::{info, warn};

use super::chat::MessageKey;
use super::fetch::{self, Fetch};
use super::registry::SlotId;

/// Voice messages longer than this (as their sender says, or as the helper
/// counts while decoding) are not recognized.
pub const MAX_VOICE_SECONDS: u64 = 300;
/// A voice file larger than this is not downloaded for recognition.
pub const MAX_VOICE_BYTES: u64 = 4 << 20;
/// One helper run at most; then it is killed.
pub const VOICE_TIMEOUT: Duration = Duration::from_secs(60);
/// Voice messages waiting for the recognition task; one more is not
/// recognized.
pub const VOICE_QUEUE: usize = 8;
/// Bytes of helper stdout read at most.
const MAX_HELPER_OUTPUT: u64 = 64 << 10;

/// What recognition made of a voice message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Heard {
    /// Its words.
    Text(String),
    /// Recognized, but no words.
    Silent,
    /// Longer than [`MAX_VOICE_SECONDS`] or larger than [`MAX_VOICE_BYTES`].
    TooLong,
    /// Not downloaded, not recognized, too late or no answer in time.
    Failed,
}

/// Turns the bytes of one OGG/Opus voice message into words: [`Helper`] in
/// the hub, a fake in tests.
pub trait Hear: Send + Sync + 'static {
    fn hear(&self, ogg: Vec<u8>) -> impl Future<Output = Heard> + Send;
}

/// The `cctg-voice` helper: `program <model>` with the voice on stdin.
#[derive(Debug, Clone)]
pub struct Helper {
    pub program: PathBuf,
    pub model: PathBuf,
    pub timeout: Duration,
}

/// The helper's answer line.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct Output {
    text: String,
    audio_ms: u64,
    took_ms: u64,
    peak_rss_kb: Option<u64>,
}

/// Environment variables the helper does not inherit: every `CCTG_*` (in
/// Docker the bot token and the hub secret are in the hub's environment)
/// and the proxies. Case is ignored.
pub fn scrubbed(name: &OsStr) -> bool {
    let name = name.as_encoded_bytes();
    name.get(..5)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case(b"CCTG_"))
        || [&b"HTTPS_PROXY"[..], b"HTTP_PROXY", b"ALL_PROXY"]
            .iter()
            .any(|proxy| name.eq_ignore_ascii_case(proxy))
}

/// The outcome of a finished helper run from its exit code and stdout.
fn heard_of(code: Option<i32>, stdout: &[u8]) -> (&'static str, Heard, Option<Output>) {
    match code {
        Some(0) => {
            let line = stdout
                .split(|byte| *byte == b'\n')
                .next()
                .unwrap_or_default();
            match serde_json::from_slice::<Output>(line) {
                Ok(output) => {
                    let text = output.text.trim();
                    let heard = if text.is_empty() {
                        Heard::Silent
                    } else {
                        Heard::Text(text.to_owned())
                    };
                    let outcome = if heard == Heard::Silent {
                        "silent"
                    } else {
                        "ok"
                    };
                    (outcome, heard, Some(output))
                }
                Err(_) => ("failed", Heard::Failed, None),
            }
        }
        Some(3) => ("too_long", Heard::TooLong, None),
        _ => ("failed", Heard::Failed, None),
    }
}

impl Helper {
    async fn run(&self, ogg: Vec<u8>) -> (&'static str, Heard, Option<Output>) {
        let mut command = tokio::process::Command::new(&self.program);
        command
            .arg(&self.model)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        for (name, _) in std::env::vars_os() {
            if scrubbed(&name) {
                command.env_remove(name);
            }
        }
        #[cfg(windows)]
        {
            // Never a window on the hub's screen.
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            command.creation_flags(CREATE_NO_WINDOW);
        }
        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(_) => return ("spawn", Heard::Failed, None),
        };
        let (Some(mut stdin), Some(mut stdout)) = (child.stdin.take(), child.stdout.take()) else {
            return ("spawn", Heard::Failed, None);
        };
        let work = async {
            // Written and read at once: a pipe holds a few KiB only.
            let feed = async move {
                let _ = stdin.write_all(&ogg).await;
                drop(stdin);
            };
            let read = async move {
                let mut output = Vec::new();
                let _ = (&mut stdout)
                    .take(MAX_HELPER_OUTPUT)
                    .read_to_end(&mut output)
                    .await;
                let _ = tokio::io::copy(&mut stdout, &mut tokio::io::sink()).await;
                output
            };
            let ((), output) = tokio::join!(feed, read);
            let status = child.wait().await;
            (output, status)
        };
        match tokio::time::timeout(self.timeout, work).await {
            Ok((output, Ok(status))) => heard_of(status.code(), &output),
            Ok((_, Err(_))) => ("failed", Heard::Failed, None),
            Err(_) => {
                let _ = child.kill().await;
                ("timeout", Heard::Failed, None)
            }
        }
    }
}

impl Hear for Helper {
    async fn hear(&self, ogg: Vec<u8>) -> Heard {
        let (outcome, heard, output) = self.run(ogg).await;
        let output = output.unwrap_or_default();
        info!(
            audio_ms = output.audio_ms,
            took_ms = output.took_ms,
            peak_rss_kb = output.peak_rss_kb,
            outcome,
            "voice recognition"
        );
        heard
    }
}

/// A voice message of `slot` to recognize; no use after `until`.
#[derive(Debug)]
pub struct Job {
    pub slot: SlotId,
    pub message: MessageKey,
    pub file_id: String,
    pub until: Instant,
}

/// Runs the jobs one at a time until the actor drops the sender; `done`
/// gets each job and what was heard. A job whose `until` passed while it
/// waited is not downloaded.
pub async fn serve<F: Fetch, H: Hear>(
    fetch: Arc<F>,
    hear: Arc<H>,
    mut jobs: mpsc::Receiver<Job>,
    done: impl Fn(Job, Heard) + Send + 'static,
) {
    while let Some(job) = jobs.recv().await {
        let heard = if job.until <= Instant::now() {
            Heard::Failed
        } else {
            match fetch::download(fetch.as_ref(), &job.file_id, MAX_VOICE_BYTES).await {
                Ok(download) => hear.hear(download.bytes).await,
                Err(error) if error.is_too_big() => Heard::TooLong,
                Err(error) => {
                    warn!(%error, "voice message not downloaded for recognition");
                    Heard::Failed
                }
            }
        };
        done(job, heard);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;
    use crate::hub::api::{ApiError, FILE_TOO_BIG};
    use crate::hub::chat::Chat;
    use crate::hub::fetch::Download;

    #[test]
    fn the_helper_never_inherits_secrets_or_proxies() {
        for name in [
            "CCTG_BOT_TOKEN",
            "cctg_hub_secret",
            "Cctg_Anything",
            "HTTPS_PROXY",
            "https_proxy",
            "HTTP_PROXY",
            "ALL_PROXY",
            "all_proxy",
        ] {
            assert!(scrubbed(OsStr::new(name)), "{name}");
        }
        for name in ["PATH", "HOME", "LANG", "SystemRoot", "CCTG", "NO_PROXY", ""] {
            assert!(!scrubbed(OsStr::new(name)), "{name}");
        }
    }

    #[test]
    fn the_answer_line_gives_the_words_or_silence() {
        let (outcome, heard, output) = heard_of(
            Some(0),
            r#"{"text":" привет \"мир\" ","audio_ms":4590,"took_ms":700,"peak_rss_kb":9}
"#
            .as_bytes(),
        );
        assert_eq!(outcome, "ok");
        assert_eq!(heard, Heard::Text("привет \"мир\"".into()));
        let output = output.unwrap();
        assert_eq!(
            (output.audio_ms, output.took_ms, output.peak_rss_kb),
            (4590, 700, Some(9))
        );
        let (outcome, heard, _) = heard_of(Some(0), br#"{"text":"  ","audio_ms":1}"#);
        assert_eq!((outcome, heard), ("silent", Heard::Silent));
        assert_eq!(heard_of(Some(0), b"not json").1, Heard::Failed);
        assert_eq!(heard_of(Some(0), b"").1, Heard::Failed);
        assert_eq!(heard_of(Some(3), b"").1, Heard::TooLong);
        for code in [Some(1), Some(2), Some(4), Some(101), None] {
            assert_eq!(
                heard_of(code, br#"{"text":"x"}"#).1,
                Heard::Failed,
                "{code:?}"
            );
        }
    }

    #[tokio::test]
    async fn a_helper_that_does_not_start_fails() {
        let helper = Helper {
            program: PathBuf::from("cctg-voice-that-does-not-exist-085"),
            model: PathBuf::from("model"),
            timeout: Duration::from_secs(5),
        };
        assert_eq!(helper.hear(b"OggS".to_vec()).await, Heard::Failed);
    }

    /// Serves `ok` and a too-big file, counts downloads.
    struct Files {
        calls: Mutex<Vec<String>>,
    }

    impl Fetch for Files {
        async fn fetch(&self, file_id: &str, limit: u64) -> Result<Download, ApiError> {
            self.calls.lock().unwrap().push(file_id.to_owned());
            assert_eq!(limit, MAX_VOICE_BYTES);
            match file_id {
                "big" => Err(ApiError::Telegram {
                    code: 400,
                    description: FILE_TOO_BIG.to_owned(),
                }),
                "gone" => Err(ApiError::Telegram {
                    code: 400,
                    description: "Bad Request: wrong file_id".to_owned(),
                }),
                _ => Ok(Download {
                    bytes: file_id.as_bytes().to_vec(),
                    path: None,
                }),
            }
        }
    }

    /// Hears the bytes as their text.
    struct Echo;

    impl Hear for Echo {
        async fn hear(&self, ogg: Vec<u8>) -> Heard {
            Heard::Text(String::from_utf8(ogg).unwrap())
        }
    }

    #[tokio::test]
    async fn jobs_are_heard_in_order_and_a_late_one_is_not_downloaded() {
        let files = Arc::new(Files {
            calls: Mutex::new(Vec::new()),
        });
        let (jobs, jobs_rx) = mpsc::channel(8);
        let (done, mut heard) = mpsc::unbounded_channel();
        tokio::spawn(serve(
            files.clone(),
            Arc::new(Echo),
            jobs_rx,
            move |job, what| {
                let _ = done.send((job.message.id, what));
            },
        ));
        let now = Instant::now();
        let job = |id: i64, file_id: &str, until: Instant| Job {
            slot: SlotId(1),
            message: MessageKey::new(Chat::GROUP, id),
            file_id: file_id.to_owned(),
            until,
        };
        let later = now + Duration::from_secs(60);
        for sent in [
            job(1, "раз", later),
            job(2, "late", now),
            job(3, "big", later),
            job(4, "gone", later),
            job(5, "два", later),
        ] {
            jobs.send(sent).await.unwrap();
        }
        let mut got = Vec::new();
        while got.len() < 5 {
            got.push(heard.recv().await.unwrap());
        }
        assert_eq!(
            got,
            [
                (1, Heard::Text("раз".into())),
                (2, Heard::Failed),
                (3, Heard::TooLong),
                (4, Heard::Failed),
                (5, Heard::Text("два".into())),
            ]
        );
        assert_eq!(*files.calls.lock().unwrap(), ["раз", "big", "gone", "два"]);
    }
}

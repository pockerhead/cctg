//! Voice messages to text on the hub (TASK-085).
//!
//! The slots actor sends a [`Job`] per voice message of a topic; one task
//! downloads the file (at most [`MAX_VOICE_BYTES`]) and hands it to the
//! helper `cctg-voice`, one voice at a time, and gets back one [`Heard`] per
//! job. The helper is one process that stays (TASK-086): it loads the model
//! baked into the hub image once, when the hub starts, and answers each
//! voice (its byte count on a line, then the OGG/Opus bytes on stdin) with
//! one JSON line. A helper that exits, answers out of the protocol or takes
//! longer than [`VOICE_TIMEOUT`] is killed; that voice goes unheard and the
//! next one starts a new helper. It never inherits the bot token, the hub
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
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout};
use tokio::sync::{Mutex, mpsc};
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
/// One voice in the helper at most (the helper's start and model load
/// included when the voice starts it); then the helper is killed.
pub const VOICE_TIMEOUT: Duration = Duration::from_secs(60);
/// Voice messages waiting for the recognition task; one more is not
/// recognized.
pub const VOICE_QUEUE: usize = 8;
/// Bytes of one helper answer line read at most.
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

    /// Gets ready for the first voice; [`serve`] calls it once, first.
    fn warm(&self) -> impl Future<Output = ()> + Send {
        async {}
    }
}

/// The `cctg-voice` helper: `program <model>`, one process kept running
/// while it answers.
#[derive(Debug)]
pub struct Helper {
    program: PathBuf,
    model: PathBuf,
    timeout: Duration,
    running: Mutex<Option<Running>>,
}

/// A started helper and its pipes.
#[derive(Debug)]
struct Running {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

/// The helper's answer line.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct Output {
    text: String,
    error: Option<String>,
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

/// The outcome of one answer line of the helper; `None` when the line is
/// no answer of its protocol.
fn heard_of(line: &[u8]) -> Option<(&'static str, Heard, Output)> {
    let output = serde_json::from_slice::<Output>(line).ok()?;
    let (outcome, heard) = match output.error.as_deref() {
        Some("too_long" | "too_big") => ("too_long", Heard::TooLong),
        Some(_) => ("failed", Heard::Failed),
        None => match output.text.trim() {
            "" => ("silent", Heard::Silent),
            text => ("ok", Heard::Text(text.to_owned())),
        },
    };
    Some((outcome, heard, output))
}

/// Writes one voice to `running` and reads its answer line; `Err` when the
/// helper is gone or its answer has no end.
async fn ask(running: &mut Running, ogg: &[u8]) -> Result<Vec<u8>, ()> {
    let header = format!("{}\n", ogg.len());
    running
        .stdin
        .write_all(header.as_bytes())
        .await
        .map_err(|_| ())?;
    running.stdin.write_all(ogg).await.map_err(|_| ())?;
    running.stdin.flush().await.map_err(|_| ())?;
    let mut line = Vec::new();
    (&mut running.stdout)
        .take(MAX_HELPER_OUTPUT)
        .read_until(b'\n', &mut line)
        .await
        .map_err(|_| ())?;
    if line.pop() != Some(b'\n') {
        return Err(());
    }
    Ok(line)
}

impl Helper {
    pub fn new(program: PathBuf, model: PathBuf, timeout: Duration) -> Self {
        Self {
            program,
            model,
            timeout,
            running: Mutex::new(None),
        }
    }

    /// A new helper process; it loads the model before it reads a voice.
    fn spawn(&self) -> Option<Running> {
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
        let Ok(mut child) = command.spawn() else {
            warn!("voice helper did not start");
            return None;
        };
        let (Some(stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
            warn!("voice helper did not start");
            return None;
        };
        info!("voice helper started");
        Some(Running {
            child,
            stdin,
            stdout: BufReader::new(stdout),
        })
    }

    /// The running helper, a new one when there is none or it exited.
    fn ready<'a>(&self, running: &'a mut Option<Running>) -> Option<&'a mut Running> {
        if let Some(current) = running.as_mut()
            && !matches!(current.child.try_wait(), Ok(None))
        {
            *running = None;
        }
        if running.is_none() {
            *running = self.spawn();
        }
        running.as_mut()
    }

    async fn run(&self, ogg: Vec<u8>) -> (&'static str, Heard, Option<Output>) {
        let mut running = self.running.lock().await;
        let Some(current) = self.ready(&mut running) else {
            return ("spawn", Heard::Failed, None);
        };
        let outcome = match tokio::time::timeout(self.timeout, ask(current, &ogg)).await {
            Ok(Ok(line)) => match heard_of(&line) {
                Some((outcome, heard, output)) => return (outcome, heard, Some(output)),
                None => "failed",
            },
            Ok(Err(())) => "crashed",
            Err(_) => "timeout",
        };
        // Gone, hung or out of step: the next voice starts a new helper.
        if let Some(mut stopped) = running.take() {
            let _ = stopped.child.kill().await;
        }
        (outcome, Heard::Failed, None)
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

    /// Starts the helper, so the model is in memory before the first voice.
    async fn warm(&self) {
        let mut running = self.running.lock().await;
        let _ = self.ready(&mut running);
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
    hear.warm().await;
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
    fn the_answer_line_gives_the_words_silence_or_the_refusal() {
        let (outcome, heard, output) = heard_of(
            r#"{"text":" привет \"мир\" ","audio_ms":4590,"took_ms":700,"peak_rss_kb":9}"#
                .as_bytes(),
        )
        .unwrap();
        assert_eq!(outcome, "ok");
        assert_eq!(heard, Heard::Text("привет \"мир\"".into()));
        assert_eq!(
            (output.audio_ms, output.took_ms, output.peak_rss_kb),
            (4590, 700, Some(9))
        );
        let (outcome, heard, _) = heard_of(br#"{"text":"  ","audio_ms":1}"#).unwrap();
        assert_eq!((outcome, heard), ("silent", Heard::Silent));
        for (error, outcome, heard) in [
            ("too_long", "too_long", Heard::TooLong),
            ("too_big", "too_long", Heard::TooLong),
            ("not_opus", "failed", Heard::Failed),
            ("failed", "failed", Heard::Failed),
            ("new", "failed", Heard::Failed),
        ] {
            let line = format!(r#"{{"error":"{error}","took_ms":3}}"#);
            let (got, what, output) = heard_of(line.as_bytes()).unwrap();
            assert_eq!((got, what), (outcome, heard), "{error}");
            assert_eq!(output.took_ms, 3);
        }
        assert!(heard_of(b"not json").is_none());
        assert!(heard_of(b"").is_none());
    }

    #[tokio::test]
    async fn a_helper_that_does_not_start_fails() {
        let helper = Helper::new(
            PathBuf::from("cctg-voice-that-does-not-exist-085"),
            PathBuf::from("model"),
            Duration::from_secs(5),
        );
        helper.warm().await;
        assert_eq!(helper.hear(b"OggS".to_vec()).await, Heard::Failed);
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

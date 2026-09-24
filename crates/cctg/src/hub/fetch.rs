//! Files of topic messages to the session (TASK-032): one task downloads
//! the Telegram file of a kept message and hands it to the session's agent
//! as `file_start` and `file_chunk`s, one file at a time for the hub.
//!
//! The slots actor sends a [`Job`] with the agent's link queue and gets one
//! [`Fetched`] back per job; the task never touches the registry. Downloads
//! do not go through the scheduler: `getFile` and the file URL are reads,
//! like `getUpdates`, not messages in the chat, so the group's message
//! budget is not theirs; a flood wait, a network error or a 5xx is tried
//! again ([`TRIES`]). Logs carry kinds and sizes, never names or bytes.

use std::collections::BTreeMap;
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use super::api::{ApiError, BotApi, FILE_TOO_BIG};
use super::buffer::Attachment;
use super::registry::SlotId;
use crate::files;
use crate::wire::HubMsg;

/// Downloads of one file: a flood wait, a network error or a Telegram 5xx
/// is tried again; a refusal is final.
const TRIES: u32 = 3;
/// Wait before trying again after a network error or a 5xx.
const RETRY_WAIT: Duration = Duration::from_secs(2);
/// Longest flood wait honoured (the other slots' files wait meanwhile).
const MAX_FLOOD_WAIT: Duration = Duration::from_secs(30);

/// A downloaded file and its Telegram path (for a default name).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Download {
    pub bytes: Vec<u8>,
    pub path: Option<String>,
}

/// Where files come from: `BotApi` in production, a fake in tests.
pub trait Fetch: Send + Sync + 'static {
    /// The file `file_id`, at most `limit` bytes; a bigger one fails with
    /// an error for which [`ApiError::is_too_big`] holds.
    fn fetch(
        &self,
        file_id: &str,
        limit: u64,
    ) -> impl Future<Output = Result<Download, ApiError>> + Send;
}

impl Fetch for BotApi {
    async fn fetch(&self, file_id: &str, limit: u64) -> Result<Download, ApiError> {
        let file = self.get_file(file_id).await?;
        if file.file_size.is_some_and(|size| size > limit) {
            return Err(ApiError::Telegram {
                code: 400,
                description: FILE_TOO_BIG.to_owned(),
            });
        }
        let Some(path) = file.file_path else {
            return Err(ApiError::Telegram {
                code: 400,
                description: "getFile gave no file_path".to_owned(),
            });
        };
        let bytes = self.download(&path, limit).await?;
        Ok(Download {
            bytes,
            path: Some(path),
        })
    }
}

/// The file of the message at the front of `slot`, for the agent behind
/// `to_agent`: `content` and `meta` as its `inbound` would carry them.
#[derive(Debug)]
pub struct Job {
    pub slot: SlotId,
    pub transfer_id: u64,
    pub file: Attachment,
    pub content: String,
    pub meta: BTreeMap<String, String>,
    pub to_agent: mpsc::Sender<HubMsg>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fetched {
    /// Every chunk is in the agent's link queue.
    Handed { size: u64 },
    /// Larger than a bot may download.
    TooBig,
    /// Telegram did not give the file.
    Failed,
    /// The agent's link closed first; the message waits for the next agent.
    LinkClosed,
}

/// Runs the jobs one at a time until the actor drops the sender; `done`
/// gets each job's slot, transfer and outcome.
pub async fn serve<F: Fetch>(
    fetch: Arc<F>,
    mut jobs: mpsc::Receiver<Job>,
    done: impl Fn(SlotId, u64, Fetched) + Send + 'static,
) {
    while let Some(job) = jobs.recv().await {
        let outcome = hand(fetch.as_ref(), &job).await;
        done(job.slot, job.transfer_id, outcome);
    }
}

async fn hand<F: Fetch>(fetch: &F, job: &Job) -> Fetched {
    // Nothing is downloaded for an agent that is gone.
    if job.to_agent.is_closed() {
        return Fetched::LinkClosed;
    }
    let kind = job.file.kind;
    let download = match download(fetch, &job.file.file_id).await {
        Ok(download) => download,
        Err(error) if error.is_too_big() => {
            info!(
                kind = kind.as_str(),
                "file from the topic too big for a bot to download"
            );
            return Fetched::TooBig;
        }
        Err(error) => {
            warn!(%error, kind = kind.as_str(), "file from the topic not downloaded");
            return Fetched::Failed;
        }
    };
    let size = download.bytes.len() as u64;
    let name = job
        .file
        .name
        .clone()
        .filter(|name| !name.trim().is_empty())
        .unwrap_or_else(|| files::default_name(kind, download.path.as_deref()));
    let start = HubMsg::FileStart {
        transfer_id: job.transfer_id,
        name,
        size,
        kind,
        content: job.content.clone(),
        meta: job.meta.clone(),
    };
    let chunks = files::chunks(job.transfer_id, &download.bytes).map(HubMsg::FileChunk);
    for message in std::iter::once(start).chain(chunks) {
        if !files::room(&job.to_agent).await || job.to_agent.send(message).await.is_err() {
            return Fetched::LinkClosed;
        }
    }
    Fetched::Handed { size }
}

/// [`Fetch::fetch`] with the tries of [`TRIES`].
async fn download<F: Fetch>(fetch: &F, file_id: &str) -> Result<Download, ApiError> {
    let mut tried = 1;
    loop {
        let error = match fetch.fetch(file_id, files::MAX_DOWNLOAD).await {
            Ok(download) => return Ok(download),
            Err(error) => error,
        };
        let wait = match &error {
            ApiError::RetryAfter(after) => (*after).min(MAX_FLOOD_WAIT),
            ApiError::Http(_) => RETRY_WAIT,
            ApiError::Telegram { code, .. } if *code >= 500 => RETRY_WAIT,
            _ => return Err(error),
        };
        if tried == TRIES {
            return Err(error);
        }
        debug!(%error, tried, "file download failed; trying again");
        tried += 1;
        tokio::time::sleep(wait).await;
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;
    use crate::wire::FileKind;

    /// Serves one file by id, counts downloads.
    struct OneFile {
        bytes: Vec<u8>,
        calls: Mutex<usize>,
    }

    impl Fetch for OneFile {
        async fn fetch(&self, file_id: &str, limit: u64) -> Result<Download, ApiError> {
            *self.calls.lock().unwrap() += 1;
            match file_id {
                "big" => Err(ApiError::Telegram {
                    code: 400,
                    description: FILE_TOO_BIG.to_owned(),
                }),
                "gone" => Err(ApiError::Telegram {
                    code: 400,
                    description: "Bad Request: wrong file_id".to_owned(),
                }),
                _ if self.bytes.len() as u64 > limit => panic!("limit ignored"),
                _ => Ok(Download {
                    bytes: self.bytes.clone(),
                    path: Some("voice/file_9.oga".into()),
                }),
            }
        }
    }

    fn job(file_id: &str, to_agent: mpsc::Sender<HubMsg>) -> Job {
        Job {
            slot: SlotId(2),
            transfer_id: 7,
            file: Attachment {
                kind: FileKind::Voice,
                file_id: file_id.into(),
                name: None,
                size: None,
            },
            content: "words".into(),
            meta: [("message_id".to_owned(), "5".to_owned())].into(),
            to_agent,
        }
    }

    #[tokio::test]
    async fn a_file_goes_to_the_agent_as_a_start_and_its_chunks() {
        let bytes: Vec<u8> = (0..files::CHUNK * 2 + 3).map(|n| (n % 7) as u8).collect();
        let fetch = OneFile {
            bytes: bytes.clone(),
            calls: Mutex::new(0),
        };
        let (to_agent, mut from_hub) = mpsc::channel(64);
        let reading = tokio::spawn(async move {
            let mut got = Vec::new();
            while let Some(msg) = from_hub.recv().await {
                got.push(msg);
            }
            got
        });
        let outcome = hand(&fetch, &job("f", to_agent)).await;
        assert_eq!(
            outcome,
            Fetched::Handed {
                size: bytes.len() as u64
            }
        );
        let got = reading.await.unwrap();
        assert_eq!(got.len(), 4);
        assert_eq!(
            got[0],
            HubMsg::FileStart {
                transfer_id: 7,
                name: "voice.oga".into(),
                size: bytes.len() as u64,
                kind: FileKind::Voice,
                content: "words".into(),
                meta: [("message_id".to_owned(), "5".to_owned())].into(),
            }
        );
        let mut assembly = files::Assembly::new(bytes.len() as u64);
        for msg in &got[1..] {
            let HubMsg::FileChunk(chunk) = msg else {
                panic!("{msg:?}");
            };
            assembly.push(chunk).unwrap();
        }
        assert_eq!(assembly.into_bytes(), bytes);
    }

    #[tokio::test]
    async fn too_big_failed_and_closed_links_are_told_apart() {
        let fetch = OneFile {
            bytes: b"x".to_vec(),
            calls: Mutex::new(0),
        };
        let (to_agent, from_hub) = mpsc::channel(8);
        assert_eq!(
            hand(&fetch, &job("big", to_agent.clone())).await,
            Fetched::TooBig
        );
        assert_eq!(
            hand(&fetch, &job("gone", to_agent.clone())).await,
            Fetched::Failed
        );
        drop(from_hub);
        // A closed link is not downloaded for.
        let calls = *fetch.calls.lock().unwrap();
        assert_eq!(hand(&fetch, &job("f", to_agent)).await, Fetched::LinkClosed);
        assert_eq!(*fetch.calls.lock().unwrap(), calls);
    }

    #[tokio::test]
    async fn a_named_file_keeps_its_name_and_a_slow_agent_holds_only_a_few_lines() {
        let bytes = vec![1u8; files::CHUNK * 10];
        let fetch = OneFile {
            bytes,
            calls: Mutex::new(0),
        };
        let (to_agent, mut from_hub) = mpsc::channel(64);
        let mut named = job("f", to_agent.clone());
        named.file.name = Some("отчёт.pdf".into());
        let handing = tokio::spawn(async move { hand(&fetch, &named).await });
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        // Nobody reads: at most AHEAD lines wait in the link queue.
        assert!(to_agent.max_capacity() - to_agent.capacity() <= files::AHEAD);
        let mut got = Vec::new();
        while got.len() < 11 {
            got.push(from_hub.recv().await.unwrap());
        }
        assert!(matches!(&got[0], HubMsg::FileStart { name, .. } if name == "отчёт.pdf"));
        assert!(matches!(handing.await.unwrap(), Fetched::Handed { .. }));
    }

    /// Fails the first `failures` downloads with `error`, then serves.
    struct Flaky {
        failures: Mutex<u32>,
        error: fn() -> ApiError,
    }

    impl Fetch for Flaky {
        async fn fetch(&self, _file_id: &str, _limit: u64) -> Result<Download, ApiError> {
            let mut failures = self.failures.lock().unwrap();
            if *failures > 0 {
                *failures -= 1;
                return Err((self.error)());
            }
            Ok(Download {
                bytes: b"ok".to_vec(),
                path: None,
            })
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_flood_wait_or_a_telegram_outage_is_tried_again() {
        // Each case its own link: nobody reads it, so a case never waits
        // for room behind the lines of another.
        let mut links = Vec::new();
        let mut link = || {
            let (to_agent, from_hub) = mpsc::channel(64);
            links.push(from_hub);
            to_agent
        };
        let flood = Flaky {
            failures: Mutex::new(1),
            error: || ApiError::RetryAfter(std::time::Duration::from_secs(3)),
        };
        assert_eq!(
            hand(&flood, &job("f", link())).await,
            Fetched::Handed { size: 2 }
        );
        let outage = Flaky {
            failures: Mutex::new(2),
            error: || ApiError::Telegram {
                code: 502,
                description: "Bad Gateway".into(),
            },
        };
        assert_eq!(
            hand(&outage, &job("f", link())).await,
            Fetched::Handed { size: 2 }
        );
        // A refusal is final; so is an outage that does not end.
        let refusal = Flaky {
            failures: Mutex::new(1),
            error: || ApiError::Telegram {
                code: 400,
                description: "Bad Request: wrong file_id".into(),
            },
        };
        assert_eq!(hand(&refusal, &job("f", link())).await, Fetched::Failed);
        let down = Flaky {
            failures: Mutex::new(u32::MAX),
            error: || ApiError::Telegram {
                code: 500,
                description: "Internal Server Error".into(),
            },
        };
        assert_eq!(hand(&down, &job("f", link())).await, Fetched::Failed);
    }
}

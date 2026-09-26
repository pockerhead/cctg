# TASK-059 edits of crates/cctg/src/hub/slots.rs (run once from the repo root).
p = 'crates/cctg/src/hub/slots.rs'
s = open(p, encoding='utf-8').read()


def rep(old, new, count=1):
    global s
    assert s.count(old) == count, (s.count(old), old[:70])
    s = s.replace(old, new)


rep('''    AgentMsg, Answered, Behavior, Client, CommandOutcome, ConsoleKey, FileChunk, FileOutcome,
    HookEvent,''', '''    AgentMsg, Answered, Behavior, Client, CommandOutcome, ConsoleKey, FileChunk, FileOutcome,
    FilePart, HookEvent,''')
rep('''/// A file from an agent being received.
struct Upload {
    transfer_id: u64,
    name: String,
    caption: Option<String>,''', '''/// A file from an agent being received.
struct Upload {
    transfer_id: u64,
    name: String,
    caption: Option<String>,
    /// The files of an album offer, in order (TASK-059); empty for one file.
    parts: Vec<FilePart>,''')
rep('''/// A job for the dispatch task.
#[derive(Debug)]
enum Work {''', '''/// The messages of an album offer on their way to Telegram (TASK-059).
struct Album {
    /// Per file of the offer: whether Telegram took it.
    sent: Vec<bool>,
    /// Messages not answered yet.
    left: usize,
}

/// A job for the dispatch task.
#[derive(Debug)]
enum Work {''')
# Work / Done variants
rep('''    File {
        conn: u64,
        transfer_id: u64,
        size: u64,
    },
}''', '''    File {
        conn: u64,
        transfer_id: u64,
        size: u64,
    },
    /// One message of an album offer: the files `parts` of it (TASK-059).
    Album {
        conn: u64,
        transfer_id: u64,
        size: u64,
        parts: Vec<usize>,
    },
}''')
rep('''    /// A file of an agent's `send_file`.
    File {
        conn: u64,
        transfer_id: u64,
        size: u64,
        delivery: Option<Delivery>,
    },
}''', '''    /// A file of an agent's `send_file`.
    File {
        conn: u64,
        transfer_id: u64,
        size: u64,
        delivery: Option<Delivery>,
    },
    /// A message of an album offer.
    Album {
        conn: u64,
        transfer_id: u64,
        size: u64,
        parts: Vec<usize>,
        delivery: Option<Delivery>,
    },
}''')
rep('''                } => Done::File {
                    conn,
                    transfer_id,
                    size,
                    delivery,
                },
            });''', '''                } => Done::File {
                    conn,
                    transfer_id,
                    size,
                    delivery,
                },
                Work::Album {
                    conn,
                    transfer_id,
                    size,
                    parts,
                } => Done::Album {
                    conn,
                    transfer_id,
                    size,
                    parts,
                    delivery,
                },
            });''')
rep('''            } => self.on_file_done(conn, transfer_id, size, delivery),
        }''', '''            } => self.on_file_done(conn, transfer_id, size, delivery),
            Done::Album {
                conn,
                transfer_id,
                size,
                parts,
                delivery,
            } => self.on_album_done(conn, transfer_id, size, &parts, delivery),
        }''')
# Slots fields
rep('''    /// Bytes of [`Self::uploads`] and of files waiting for Telegram.
    file_bytes: u64,''', '''    /// Bytes of [`Self::uploads`] and of files waiting for Telegram.
    file_bytes: u64,
    /// Album offers waiting for Telegram, by `(conn, transfer_id)`.
    albums: HashMap<(u64, u64), Album>,''')
rep('''            uploads: HashMap::new(),
            file_bytes: 0,''', '''            uploads: HashMap::new(),
            file_bytes: 0,
            albums: HashMap::new(),''')
# routing
rep('''                    AgentMsg::FileOffer {
                        transfer_id,
                        name,
                        size,
                        caption,
                    } => self.on_file_offer(conn, &session, transfer_id, name, size, caption),''', '''                    AgentMsg::FileOffer {
                        transfer_id,
                        name,
                        size,
                        caption,
                        parts,
                    } => {
                        let offer = Offer {
                            transfer_id,
                            name,
                            size,
                            caption,
                            parts,
                        };
                        self.on_file_offer(conn, &session, offer);
                    }''')
rep('''    fn on_file_offer(
        &mut self,
        conn: u64,
        frame_session: &str,
        transfer_id: u64,
        name: String,
        size: u64,
        caption: Option<String>,
    ) {
        if let Some(old)''', '''    fn on_file_offer(&mut self, conn: u64, frame_session: &str, offer: Offer) {
        let Offer {
            transfer_id,
            name,
            size,
            caption,
            parts,
        } = offer;
        if let Some(old)''')
rep('''        let outcome = if target.is_none() {
            FileOutcome::NoTopic
        } else if size == 0 || size > files::MAX_UPLOAD {
            FileOutcome::Failed''', '''        let outcome = if target.is_none() {
            FileOutcome::NoTopic
        } else if size == 0 || size > files::MAX_UPLOAD || !album_fits(&parts, size) {
            FileOutcome::Failed''')
rep('''        info!(conn, size, ?outcome, "file offered by an agent");
        if outcome == FileOutcome::Accepted {
            self.file_bytes += size;
            self.uploads.insert(
                conn,
                Upload {
                    transfer_id,
                    name,
                    caption,
                    assembly''', '''        info!(
            conn,
            size,
            files = parts.len().max(1),
            ?outcome,
            "file offered by an agent"
        );
        if outcome == FileOutcome::Accepted {
            self.file_bytes += size;
            self.uploads.insert(
                conn,
                Upload {
                    transfer_id,
                    name,
                    caption,
                    parts,
                    assembly''')
rep('''        let (Some((slot, thread_id)), None) = (target, refused) else {
            self.file_bytes = self.file_bytes.saturating_sub(size);
            let outcome = refused.unwrap_or(FileOutcome::NoTopic);
            info!(conn, size, ?outcome, "file from an agent not sent");
            self.answer_file(conn, upload.transfer_id, outcome);
            return;
        };
        let bytes = upload.assembly.into_bytes();''', '''        let (Some((slot, thread_id)), None) = (target, refused) else {
            self.file_bytes = self.file_bytes.saturating_sub(size);
            let outcome = refused.unwrap_or(FileOutcome::NoTopic);
            info!(conn, size, ?outcome, "file from an agent not sent");
            self.answer_file(conn, upload.transfer_id, outcome);
            return;
        };
        if !upload.parts.is_empty() {
            self.send_album(conn, slot, thread_id, upload);
            return;
        }
        let bytes = upload.assembly.into_bytes();''')
rep('''    fn answer_file(&self, conn: u64, transfer_id: u64, outcome: FileOutcome) {
        let answer = HubMsg::FileAnswer {
            transfer_id,
            outcome,
            parts: Vec::new(),
        };''', '''    /// The files of a complete album offer (TASK-059) go to the topic:
    /// the pictures as a photo album, then the rest as a document album
    /// (Telegram never mixes the two); a kind with one file goes alone, as
    /// with `path`. The caption goes on the first file; without one each
    /// photo shows its file name (TASK-051). One message token per message.
    fn send_album(&mut self, conn: u64, slot: SlotId, thread_id: i64, upload: Upload) {
        let size = upload.assembly.size();
        let bytes = upload.assembly.into_bytes();
        let mut photos = Vec::new();
        let mut others = Vec::new();
        let mut offset = 0usize;
        for (index, part) in upload.parts.iter().enumerate() {
            let end = offset + part.size as usize;
            let bytes = bytes[offset..end].to_vec();
            offset = end;
            let photo = part.size <= files::MAX_PHOTO && files::is_photo(&bytes);
            let file_name = files::clean_name(&part.name, "file");
            let caption = match &upload.caption {
                Some(_) => None,
                None if photo => Some(cut(&file_name, CAPTION_LIMIT)),
                None => None,
            };
            let document = Document {
                file_name,
                bytes,
                caption,
            };
            if photo {
                photos.push((index, document));
            } else {
                others.push((index, document));
            }
        }
        let mut ops: Vec<(Vec<usize>, u64, Op)> = Vec::new();
        for (group, photo) in [(photos, true), (others, false)] {
            if group.is_empty() {
                continue;
            }
            let bytes = group.iter().map(|(_, doc)| doc.bytes.len() as u64).sum();
            let (parts, mut items): (Vec<usize>, Vec<Document>) = group.into_iter().unzip();
            if ops.is_empty()
                && let (Some(caption), Some(first)) = (&upload.caption, items.first_mut())
            {
                first.caption = Some(cut(caption, CAPTION_LIMIT));
            }
            let thread_id = Some(thread_id);
            let op = match items.len() {
                1 => {
                    let document = items.remove(0);
                    if photo {
                        Op::SendPhoto {
                            thread_id,
                            document,
                            notify: false,
                        }
                    } else {
                        Op::SendDocument {
                            thread_id,
                            document,
                            notify: false,
                        }
                    }
                }
                _ => Op::SendAlbum {
                    thread_id,
                    items,
                    photos: photo,
                    notify: false,
                },
            };
            ops.push((parts, bytes, op));
        }
        // `album_fits` checked that the sizes add up to the bytes.
        debug_assert_eq!(offset as u64, size);
        self.queued_messages += ops.len();
        info!(
            ordinal = self.ordinal(slot),
            size,
            files = upload.parts.len(),
            messages = ops.len(),
            "album from the session queued for its topic"
        );
        self.albums.insert(
            (conn, upload.transfer_id),
            Album {
                sent: vec![false; upload.parts.len()],
                left: ops.len(),
            },
        );
        for (parts, bytes, op) in ops {
            self.hand_off(
                Work::Album {
                    conn,
                    transfer_id: upload.transfer_id,
                    size: bytes,
                    parts,
                },
                op,
            );
        }
    }

    /// Telegram answered one message of an album offer; after the last one
    /// the agent hears which files went.
    fn on_album_done(
        &mut self,
        conn: u64,
        transfer_id: u64,
        size: u64,
        parts: &[usize],
        delivery: Option<Delivery>,
    ) {
        self.queued_messages = self.queued_messages.saturating_sub(1);
        if self.queued_messages == 0 {
            self.overflow_warned = false;
        }
        self.file_bytes = self.file_bytes.saturating_sub(size);
        let went = matches!(delivery, Some(Ok(_)));
        match delivery {
            Some(Ok(_)) => info!(conn, size, "album message from the session sent to its topic"),
            Some(Err(error)) => warn!(%error, size, "album message from the session not delivered"),
            None => warn!(size, "album message from the session got no answer"),
        }
        let Some(album) = self.albums.get_mut(&(conn, transfer_id)) else {
            return;
        };
        for &part in parts {
            if let Some(sent) = album.sent.get_mut(part) {
                *sent = went;
            }
        }
        album.left = album.left.saturating_sub(1);
        if album.left > 0 {
            return;
        }
        let Some(album) = self.albums.remove(&(conn, transfer_id)) else {
            return;
        };
        let parts: Vec<FileOutcome> = album
            .sent
            .iter()
            .map(|&sent| {
                if sent {
                    FileOutcome::Sent
                } else {
                    FileOutcome::Failed
                }
            })
            .collect();
        let outcome = if album.sent.contains(&true) {
            FileOutcome::Sent
        } else {
            FileOutcome::Failed
        };
        self.answer(conn, transfer_id, outcome, parts);
    }

    fn answer_file(&self, conn: u64, transfer_id: u64, outcome: FileOutcome) {
        self.answer(conn, transfer_id, outcome, Vec::new());
    }

    fn answer(&self, conn: u64, transfer_id: u64, outcome: FileOutcome, parts: Vec<FileOutcome>) {
        let answer = HubMsg::FileAnswer {
            transfer_id,
            outcome,
            parts,
        };''')
# helper types near Upload
rep('''impl Default for Options {
    fn default() -> Self {''', '''/// A `file_offer` as it came.
struct Offer {
    transfer_id: u64,
    name: String,
    size: u64,
    caption: Option<String>,
    parts: Vec<FilePart>,
}

/// An offer's parts, if any, are 2 to [`MAX_ALBUM`] files of 1 byte to
/// [`files::MAX_UPLOAD`] each that add up to its size.
fn album_fits(parts: &[FilePart], size: u64) -> bool {
    parts.is_empty()
        || ((2..=MAX_ALBUM).contains(&parts.len())
            && parts
                .iter()
                .all(|part| part.size > 0 && part.size <= files::MAX_UPLOAD)
            && parts.iter().map(|part| part.size).sum::<u64>() == size)
}

impl Default for Options {
    fn default() -> Self {''')
rep('''    FilePart, HookEvent,''', '''    FilePart, HookEvent, MAX_ALBUM,''')
# test helper pattern
rep('''            if let HubMsg::FileAnswer {
                transfer_id,
                outcome,
            } = msg''', '''            if let HubMsg::FileAnswer {
                transfer_id,
                outcome,
                ..
            } = msg''')
open(p, 'w', encoding='utf-8', newline='').write(s)
print('ok')

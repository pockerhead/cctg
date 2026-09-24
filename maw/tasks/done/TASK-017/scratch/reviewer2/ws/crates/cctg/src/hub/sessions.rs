//! Which transcript `/brief` and `/full` read.
//!
//! `TranscriptLocator` is the seam. `SlotLocator` answers a command in a slot
//! topic with the current session of that slot; everything else (General, a
//! session id prefix, a topic the registry does not know) goes to
//! `ProjectsDir`, which scans the Claude Code projects directory.

use std::io;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use tokio::sync::watch;

use super::registry::TopicView;

/// A top-level session transcript.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Located {
    pub session_id: String,
    /// Encoded-cwd directory name, e.g. `C--Users-me-dev-app`. Internal only:
    /// it contains a private path and is never sent to Telegram or logged.
    pub project: String,
    pub path: PathBuf,
}

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum LocateError {
    #[error("the projects directory does not exist")]
    RootMissing,
    #[error("the projects directory cannot be read ({0:?})")]
    RootUnreadable(io::ErrorKind),
    #[error("no sessions")]
    NoSessions,
    #[error("no session id starts with the given prefix")]
    NoMatch,
    /// Every match, newest first.
    #[error("the prefix matches {} sessions", .0.len())]
    Ambiguous(Vec<Located>),
    /// The topic's session was announced without a transcript path.
    #[error("the session of this topic has no known transcript")]
    NoTranscript,
}

pub trait TranscriptLocator: Send + Sync + 'static {
    /// The transcript for a command sent in `thread_id` (`None` is General),
    /// narrowed to sessions whose id starts with `session_prefix` when given.
    fn locate(
        &self,
        thread_id: Option<i64>,
        session_prefix: Option<&str>,
    ) -> Result<Located, LocateError>;
}

/// Finds `<root>/<project>/<session-id>.jsonl`. Only direct children of a
/// project directory count, so `<session-id>/subagents/*.jsonl` never does.
#[derive(Debug)]
pub struct ProjectsDir {
    root: PathBuf,
}

impl ProjectsDir {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    /// All top-level sessions, newest first; ties by id.
    fn sessions(&self) -> Result<Vec<Located>, LocateError> {
        let projects = match std::fs::read_dir(&self.root) {
            Ok(projects) => projects,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Err(LocateError::RootMissing);
            }
            Err(error) => return Err(LocateError::RootUnreadable(error.kind())),
        };
        let mut sessions: Vec<(SystemTime, Located)> = Vec::new();
        for project in projects.flatten() {
            if !project.file_type().is_ok_and(|kind| kind.is_dir()) {
                continue;
            }
            let Ok(files) = std::fs::read_dir(project.path()) else {
                continue;
            };
            let project_name = project.file_name().to_string_lossy().into_owned();
            for file in files.flatten() {
                let name = file.file_name();
                let Some(session_id) = name
                    .to_str()
                    .and_then(|name| name.strip_suffix(".jsonl"))
                    .filter(|id| is_session_id(id))
                else {
                    continue;
                };
                let Ok(metadata) = file.metadata() else {
                    continue;
                };
                if !metadata.is_file() {
                    continue;
                }
                let modified = metadata.modified().unwrap_or(UNIX_EPOCH);
                sessions.push((
                    modified,
                    Located {
                        session_id: session_id.to_owned(),
                        project: project_name.clone(),
                        path: file.path(),
                    },
                ));
            }
        }
        sessions.sort_by(|(a_time, a), (b_time, b)| {
            b_time
                .cmp(a_time)
                .then_with(|| a.session_id.cmp(&b.session_id))
        });
        Ok(sessions.into_iter().map(|(_, session)| session).collect())
    }
}

impl TranscriptLocator for ProjectsDir {
    fn locate(
        &self,
        _thread_id: Option<i64>,
        session_prefix: Option<&str>,
    ) -> Result<Located, LocateError> {
        let mut sessions = self.sessions()?;
        let Some(prefix) = session_prefix else {
            return sessions.into_iter().next().ok_or(LocateError::NoSessions);
        };
        sessions.retain(|session| session.session_id.starts_with(prefix));
        match sessions.len() {
            0 => Err(LocateError::NoMatch),
            1 => Ok(sessions.remove(0)),
            _ => Err(LocateError::Ambiguous(sessions)),
        }
    }
}

/// The slot registry's view first, `ProjectsDir` for the rest.
#[derive(Debug)]
pub struct SlotLocator {
    view: watch::Receiver<Arc<TopicView>>,
    fallback: ProjectsDir,
}

impl SlotLocator {
    pub fn new(view: watch::Receiver<Arc<TopicView>>, fallback: ProjectsDir) -> Self {
        Self { view, fallback }
    }
}

impl TranscriptLocator for SlotLocator {
    fn locate(
        &self,
        thread_id: Option<i64>,
        session_prefix: Option<&str>,
    ) -> Result<Located, LocateError> {
        let current = match (thread_id, session_prefix) {
            (Some(thread_id), None) => self.view.borrow().get(&thread_id).cloned(),
            _ => None,
        };
        match current {
            Some((_, path)) if path.is_empty() => Err(LocateError::NoTranscript),
            Some((session_id, path)) => Ok(Located {
                session_id,
                project: String::new(),
                path: PathBuf::from(path),
            }),
            None => self.fallback.locate(thread_id, session_prefix),
        }
    }
}

/// Claude Code names top-level transcripts by a lowercase UUID.
fn is_session_id(id: &str) -> bool {
    id.len() == 36
        && id.char_indices().all(|(index, c)| match index {
            8 | 13 | 18 | 23 => c == '-',
            _ => c.is_ascii_digit() || ('a'..='f').contains(&c),
        })
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::time::Duration;

    use super::*;
    use crate::hub::testdir::TempDir;

    const OLD: &str = "0a1b2c3d-0000-4000-8000-000000000001";
    const NEW: &str = "0a1b9999-0000-4000-8000-000000000002";
    const OTHER: &str = "f0000000-0000-4000-8000-000000000003";

    fn write(path: &Path, age_secs: u64) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, "{}\n").unwrap();
        let modified = SystemTime::now() - Duration::from_secs(age_secs);
        std::fs::File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(modified)
            .unwrap();
    }

    fn root() -> TempDir {
        let dir = TempDir::new("sessions");
        write(
            &dir.path().join("C--proj-a").join(format!("{OLD}.jsonl")),
            300,
        );
        write(
            &dir.path().join("C--proj-b").join(format!("{NEW}.jsonl")),
            200,
        );
        write(
            &dir.path().join("C--proj-b").join(format!("{OTHER}.jsonl")),
            100,
        );
        dir
    }

    fn locate(dir: &TempDir, prefix: Option<&str>) -> Result<Located, LocateError> {
        ProjectsDir::new(dir.path().to_owned()).locate(None, prefix)
    }

    #[test]
    fn newest_session_wins_without_a_prefix() {
        let dir = root();
        let found = locate(&dir, None).unwrap();
        assert_eq!(found.session_id, OTHER);
        assert_eq!(found.project, "C--proj-b");
        assert_eq!(
            found.path,
            dir.path().join("C--proj-b").join(format!("{OTHER}.jsonl"))
        );
    }

    #[test]
    fn prefix_selects_one_or_lists_candidates() {
        let dir = root();
        assert_eq!(locate(&dir, Some("0a1b2")).unwrap().session_id, OLD);
        assert_eq!(locate(&dir, Some(OLD)).unwrap().session_id, OLD);
        match locate(&dir, Some("0a1b")) {
            Err(LocateError::Ambiguous(candidates)) => {
                let ids: Vec<&str> = candidates.iter().map(|c| c.session_id.as_str()).collect();
                assert_eq!(ids, [NEW, OLD], "newest first");
            }
            other => panic!("expected candidates, got {other:?}"),
        }
        assert_eq!(locate(&dir, Some("dead")), Err(LocateError::NoMatch));
    }

    #[test]
    fn subagent_transcripts_are_never_sessions() {
        let dir = root();
        let nested = dir.path().join("C--proj-b").join(OTHER).join("subagents");
        // Newer than every top-level file, and one of them named like a session.
        write(&nested.join("agent-a0123.jsonl"), 0);
        write(
            &nested.join("ffffffff-0000-4000-8000-000000000009.jsonl"),
            0,
        );
        assert_eq!(locate(&dir, None).unwrap().session_id, OTHER);
        assert_eq!(locate(&dir, Some("ffffffff")), Err(LocateError::NoMatch));
    }

    #[test]
    fn non_session_entries_are_skipped() {
        let dir = TempDir::new("sessions-junk");
        let project = dir.path().join("C--proj");
        write(&project.join("notes.jsonl"), 0);
        write(&project.join("sessions-index.json"), 0);
        write(&dir.path().join(format!("{NEW}.jsonl")), 0);
        std::fs::create_dir_all(project.join(format!("{OTHER}.jsonl"))).unwrap();
        assert_eq!(locate(&dir, None), Err(LocateError::NoSessions));
        write(&project.join(format!("{OLD}.jsonl")), 50);
        assert_eq!(locate(&dir, None).unwrap().session_id, OLD);
    }

    #[test]
    fn missing_root_is_reported() {
        let dir = TempDir::new("sessions-missing");
        let missing = ProjectsDir::new(dir.path().join("absent"));
        assert_eq!(missing.locate(None, None), Err(LocateError::RootMissing));
    }

    #[test]
    fn a_slot_topic_reads_its_current_session() {
        let dir = root();
        let view: TopicView = [
            (7, (NEW.to_owned(), "/somewhere/new.jsonl".to_owned())),
            (8, (OLD.to_owned(), String::new())),
        ]
        .into();
        let (_tx, rx) = watch::channel(Arc::new(view));
        let locator = SlotLocator::new(rx, ProjectsDir::new(dir.path().to_owned()));
        let found = locator.locate(Some(7), None).unwrap();
        assert_eq!(found.session_id, NEW);
        assert_eq!(found.path, PathBuf::from("/somewhere/new.jsonl"));
        assert_eq!(
            locator.locate(Some(8), None),
            Err(LocateError::NoTranscript)
        );
        // General, unknown topics and prefixes keep the directory scan.
        assert_eq!(locator.locate(None, None).unwrap().session_id, OTHER);
        assert_eq!(locator.locate(Some(9), None).unwrap().session_id, OTHER);
        assert_eq!(
            locator.locate(Some(7), Some("0a1b2")).unwrap().session_id,
            OLD
        );
    }

    #[test]
    fn session_id_shape() {
        assert!(is_session_id(OLD));
        assert!(!is_session_id("0A1B2C3D-0000-4000-8000-000000000001"));
        assert!(!is_session_id("agent-a0123"));
        assert!(!is_session_id("0a1b2c3d00000-4000-8000-000000000001"));
    }
}

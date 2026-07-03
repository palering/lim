//! Session discovery and metadata.
//!
//! [`SessionManager`] scans the sessions directory and derives
//! [`SessionMeta`] from each JSONL file — no sidecar metadata files, no
//! database. All metadata is computed from the events themselves so the
//! source of truth is always the JSONL file.
//!
//! ## Performance
//!
//! - [`SessionManager::list`] scans all `.jsonl` files but reads only the
//!   first line (for session_id) + file metadata (size, mtime). It does
//!   NOT parse every event — suitable for hundreds of sessions.
//! - [`SessionManager::meta`] reads the full JSONL to compute message count,
//!   token totals, and title. Call this on demand (e.g. when the user opens
//!   a session detail view), not for every list() entry.

use std::time::SystemTime;

use camino::{Utf8Path, Utf8PathBuf};
use tokio::fs;

use lim_core::error::{LimError, Result};
use lim_core::event::{AgentEvent, EventEnvelope};
use lim_core::id::SessionId;
use lim_core::message::{AgentMessage, Role};

use crate::reader::SessionReader;

/// Manages a directory of session JSONL files.
///
/// Stateless: every call reads from disk. Suitable for interactive use
/// (CLI/TUI); not optimized for high-throughput querying.
pub struct SessionManager {
    sessions_dir: Utf8PathBuf,
}

/// Lightweight metadata about a session, derived from the JSONL file
/// without reading every event.
///
/// Returned by [`SessionManager::list`]. Use [`SessionManager::meta`] to
/// load the full metadata (message count, token totals, title).
#[derive(Debug, Clone)]
pub struct SessionMeta {
    /// Session ID parsed from the first event in the JSONL.
    pub session_id: SessionId,
    /// Path to the `.jsonl` file.
    pub path: Utf8PathBuf,
    /// File creation time (best-effort: falls back to mtime).
    pub created_at: SystemTime,
    /// File last-modified time.
    pub updated_at: SystemTime,
    /// File size in bytes.
    pub size_bytes: u64,
    /// Number of events in the JSONL (line count). Cheap to compute.
    pub event_count: usize,
}

/// Detailed metadata, computed by scanning every event in the JSONL.
///
/// Use this when the user opens a session detail view.
#[derive(Debug, Clone)]
pub struct SessionDetails {
    /// Basic metadata (path, IDs, timestamps).
    pub meta: SessionMeta,
    /// Total number of messages (from `MessageEnd` events).
    pub message_count: usize,
    /// Total input tokens across all turns (from `TurnEnd::usage`).
    pub total_input_tokens: u64,
    /// Total output tokens across all turns.
    pub total_output_tokens: u64,
    /// First user message text, truncated to 80 chars. Used as a title.
    pub title: String,
}

impl SessionManager {
    /// Create a new manager pointing at the given sessions directory.
    /// The directory is created lazily on first write.
    pub fn new(sessions_dir: impl Into<Utf8PathBuf>) -> Self {
        Self { sessions_dir: sessions_dir.into() }
    }

    /// The configured sessions directory.
    pub fn sessions_dir(&self) -> &Utf8Path {
        &self.sessions_dir
    }

    /// List all sessions in the directory, sorted by `updated_at` descending
    /// (most recent first).
    ///
    /// Reads only the first line of each `.jsonl` file (for the session ID)
    /// and file metadata (size, mtime). Does NOT compute message or token
    /// counts — use [`SessionManager::details`] for those.
    pub async fn list(&self) -> Result<Vec<SessionMeta>> {
        self.ensure_dir().await?;

        let mut entries = fs::read_dir(self.sessions_dir.as_std_path()).await.map_err(|e| {
            LimError::tool_execution(
                "session_manager",
                format!("failed to read sessions dir `{}`: {e}", self.sessions_dir),
                e,
            )
        })?;

        let mut metas = Vec::new();
        while let Some(entry) = entries.next_entry().await.map_err(|e| {
            LimError::tool_execution("session_manager", format!("failed to read dir entry: {e}"), e)
        })? {
            let path = entry.path();
            // Skip non-`.jsonl` files (e.g. editor swap files, hidden files).
            if !path.is_file() {
                continue;
            }
            let path_str = match path.to_str() {
                Some(s) => s,
                None => continue,
            };
            if !path_str.ends_with(".jsonl") {
                continue;
            }
            let utf8_path = Utf8PathBuf::from(path_str);

            match self.derive_meta(&utf8_path).await {
                Ok(meta) => metas.push(meta),
                Err(e) => {
                    tracing::warn!(
                        path = %utf8_path,
                        error = %e,
                        "skipping session file: failed to derive metadata"
                    );
                }
            }
        }

        // Sort by updated_at descending (most recent first).
        metas.sort_by_key(|a| std::cmp::Reverse(a.updated_at));
        Ok(metas)
    }

    /// Find a session by its ID. Scans the sessions directory.
    ///
    /// Returns `Ok(None)` if not found.
    pub async fn find(&self, session_id: &SessionId) -> Result<Option<SessionMeta>> {
        let list = self.list().await?;
        Ok(list.into_iter().find(|m| &m.session_id == session_id))
    }

    /// Load detailed metadata for a session (message count, tokens, title).
    ///
    /// Reads the full JSONL file. Use [`Self::list`] for cheap enumeration.
    pub async fn details(&self, path: &Utf8Path) -> Result<SessionDetails> {
        let meta = self.derive_meta(path).await?;
        let events = SessionReader::read_all(path).await?;

        let mut message_count = 0usize;
        let mut total_input_tokens = 0u64;
        let mut total_output_tokens = 0u64;
        let mut title = String::new();

        for envelope in &events {
            match &envelope.event {
                AgentEvent::MessageEnd { message, .. } => {
                    message_count += 1;
                    if title.is_empty() && message.role == Role::User {
                        title = extract_title(message);
                    }
                }
                AgentEvent::TurnEnd { usage, .. } => {
                    total_input_tokens += usage.input_tokens;
                    total_output_tokens += usage.output_tokens;
                }
                _ => {}
            }
        }

        if title.is_empty() {
            title = "(no user message)".to_string();
        }

        Ok(SessionDetails { meta, message_count, total_input_tokens, total_output_tokens, title })
    }

    /// Delete a session file. Returns `Ok(false)` if the file does not exist.
    pub async fn delete(&self, path: &Utf8Path) -> Result<bool> {
        match fs::remove_file(path.as_std_path()).await {
            Ok(()) => Ok(true),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(LimError::tool_execution(
                "session_manager",
                format!("failed to delete `{path}`: {e}"),
                e,
            )),
        }
    }

    /// Create the sessions directory if it doesn't exist.
    async fn ensure_dir(&self) -> Result<()> {
        fs::create_dir_all(self.sessions_dir.as_std_path()).await.map_err(|e| {
            LimError::tool_execution(
                "session_manager",
                format!("failed to create sessions dir `{}`: {e}", self.sessions_dir),
                e,
            )
        })
    }

    /// Derive basic metadata from a single JSONL file.
    ///
    /// Reads only the first line (for session_id) + file stat.
    async fn derive_meta(&self, path: &Utf8Path) -> Result<SessionMeta> {
        let metadata = fs::metadata(path.as_std_path()).await.map_err(|e| {
            LimError::tool_execution("session_manager", format!("failed to stat `{path}`: {e}"), e)
        })?;

        let created_at = metadata.created().unwrap_or_else(|_| SystemTime::now());
        let updated_at = metadata.modified().unwrap_or_else(|_| SystemTime::now());
        let size_bytes = metadata.len();

        // Read first line for session_id.
        let session_id = read_first_session_id(path).await?.unwrap_or_else(SessionId::new);

        // Count lines for event_count (cheap: just count '\n').
        let event_count = count_lines(path).await;

        Ok(SessionMeta {
            session_id,
            path: path.to_path_buf(),
            created_at,
            updated_at,
            size_bytes,
            event_count,
        })
    }
}

/// Read the first event envelope from a JSONL file and extract its session_id.
///
/// Returns `Ok(None)` if the file is empty or the first line is unparseable.
async fn read_first_session_id(path: &Utf8Path) -> Result<Option<SessionId>> {
    let content = match fs::read_to_string(path.as_std_path()).await {
        Ok(c) => c,
        Err(_) => return Ok(None),
    };
    let Some(first_line) = content.lines().next() else {
        return Ok(None);
    };
    if first_line.trim().is_empty() {
        return Ok(None);
    }
    let envelope: EventEnvelope = match serde_json::from_str(first_line) {
        Ok(e) => e,
        Err(_) => return Ok(None),
    };
    Ok(Some(envelope.session_id))
}

/// Count lines in a file by counting `\n` bytes.
///
/// Used as a fast event_count proxy without parsing JSON.
async fn count_lines(path: &Utf8Path) -> usize {
    match fs::read(path.as_std_path()).await {
        Ok(bytes) => bytes.iter().filter(|&&b| b == b'\n').count(),
        Err(_) => 0,
    }
}

/// Extract a title from a user message: first non-empty line, truncated.
fn extract_title(msg: &AgentMessage) -> String {
    let text: String = msg
        .content
        .iter()
        .filter_map(|block| {
            if let lim_core::content::ContentBlock::Text { text } = block {
                Some(text.as_str())
            } else {
                None
            }
        })
        .collect::<Vec<_>>()
        .join(" ");

    let first_line = text.lines().next().unwrap_or("").trim();
    let truncated = if first_line.chars().count() > 80 {
        let mut end = 0;
        for (i, (byte_idx, _)) in first_line.char_indices().enumerate() {
            if i >= 80 {
                end = byte_idx;
                break;
            }
        }
        format!("{}…", &first_line[..end])
    } else {
        first_line.to_string()
    };

    if truncated.is_empty() { "(empty message)".to_string() } else { truncated }
}

#[cfg(test)]
mod tests {
    use super::*;
    use camino::Utf8PathBuf;
    use lim_core::event::{AgentEvent, EventEnvelope};
    use lim_core::id::{BranchId, SessionId, TraceId};
    use lim_core::message::AgentMessage;
    use lim_core::usage::Usage;

    async fn write_session(dir: &Utf8Path, name: &str, session_id: SessionId) -> Utf8PathBuf {
        let path = dir.join(name);
        let mut writer = crate::writer::SessionWriter::create(&path).await.unwrap();
        // Write a minimal envelope.
        let envelope = EventEnvelope::new(
            1,
            session_id,
            BranchId::new(),
            TraceId::new(),
            AgentEvent::AgentStart,
        );
        writer.write_event(&envelope).await.unwrap();
        path
    }

    async fn write_session_with_messages(
        dir: &Utf8Path,
        name: &str,
        session_id: SessionId,
    ) -> Utf8PathBuf {
        let path = dir.join(name);
        let mut writer = crate::writer::SessionWriter::create(&path).await.unwrap();

        // AgentStart
        writer
            .write_event(&EventEnvelope::new(
                1,
                session_id,
                BranchId::new(),
                TraceId::new(),
                AgentEvent::AgentStart,
            ))
            .await
            .unwrap();

        // User message
        let user_msg = AgentMessage::user("Hello, this is a long title that should be truncated");
        writer
            .write_event(&EventEnvelope::new(
                2,
                session_id,
                BranchId::new(),
                TraceId::new(),
                AgentEvent::MessageEnd { message_id: user_msg.id, message: user_msg.clone() },
            ))
            .await
            .unwrap();

        // TurnEnd with usage
        writer
            .write_event(&EventEnvelope::new(
                3,
                session_id,
                BranchId::new(),
                TraceId::new(),
                AgentEvent::TurnEnd {
                    turn_id: lim_core::id::TurnId::new(),
                    stop_reason: lim_core::stop_reason::StopReason::EndTurn,
                    usage: Usage::new(100, 50),
                },
            ))
            .await
            .unwrap();

        path
    }

    #[tokio::test]
    async fn list_returns_empty_for_empty_dir() {
        let dir = std::env::temp_dir().join(format!("lim-test-sm-empty-{}", uuid_v4()));
        let _ = std::fs::create_dir_all(&dir);
        let utf8_dir = Utf8PathBuf::try_from(dir.clone()).unwrap();

        let sm = SessionManager::new(&utf8_dir);
        let list = sm.list().await.unwrap();
        assert!(list.is_empty());

        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn list_returns_sessions_sorted_by_mtime_desc() {
        let dir = std::env::temp_dir().join(format!("lim-test-sm-list-{}", uuid_v4()));
        let _ = std::fs::create_dir_all(&dir);
        let utf8_dir = Utf8PathBuf::try_from(dir.clone()).unwrap();

        let id_a = SessionId::new();
        let id_b = SessionId::new();
        write_session(&utf8_dir, "a.jsonl", id_a).await;
        // tiny delay so b.jsonl has a later mtime
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        write_session(&utf8_dir, "b.jsonl", id_b).await;

        let sm = SessionManager::new(&utf8_dir);
        let list = sm.list().await.unwrap();
        assert_eq!(list.len(), 2);
        // Most recent first.
        assert_eq!(list[0].session_id, id_b);
        assert_eq!(list[1].session_id, id_a);

        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn find_by_session_id() {
        let dir = std::env::temp_dir().join(format!("lim-test-sm-find-{}", uuid_v4()));
        let _ = std::fs::create_dir_all(&dir);
        let utf8_dir = Utf8PathBuf::try_from(dir.clone()).unwrap();

        let id = SessionId::new();
        let path = write_session(&utf8_dir, "target.jsonl", id).await;
        write_session(&utf8_dir, "other.jsonl", SessionId::new()).await;

        let sm = SessionManager::new(&utf8_dir);
        let found = sm.find(&id).await.unwrap();
        assert!(found.is_some(), "session should be found by ID");
        assert_eq!(found.unwrap().path, path);

        let missing = sm.find(&SessionId::new()).await.unwrap();
        assert!(missing.is_none(), "nonexistent session should return None");

        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn details_computes_counts_and_title() {
        let dir = std::env::temp_dir().join(format!("lim-test-sm-details-{}", uuid_v4()));
        let _ = std::fs::create_dir_all(&dir);
        let utf8_dir = Utf8PathBuf::try_from(dir.clone()).unwrap();

        let id = SessionId::new();
        let path = write_session_with_messages(&utf8_dir, "rich.jsonl", id).await;

        let sm = SessionManager::new(&utf8_dir);
        let details = sm.details(&path).await.unwrap();

        assert_eq!(details.meta.session_id, id);
        assert_eq!(details.message_count, 1, "one MessageEnd event");
        assert_eq!(details.total_input_tokens, 100);
        assert_eq!(details.total_output_tokens, 50);
        assert!(
            details.title.contains("Hello, this is a long title"),
            "title should be extracted from first user message, got: {}",
            details.title
        );

        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn delete_removes_file() {
        let dir = std::env::temp_dir().join(format!("lim-test-sm-delete-{}", uuid_v4()));
        let _ = std::fs::create_dir_all(&dir);
        let utf8_dir = Utf8PathBuf::try_from(dir.clone()).unwrap();

        let path = write_session(&utf8_dir, "doomed.jsonl", SessionId::new()).await;
        assert!(path.exists());

        let sm = SessionManager::new(&utf8_dir);
        let deleted = sm.delete(&path).await.unwrap();
        assert!(deleted, "delete should return true for existing file");
        assert!(!path.exists(), "file should be gone");

        let deleted_again = sm.delete(&path).await.unwrap();
        assert!(!deleted_again, "delete on missing file should return false");

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn extract_title_truncates_long_text() {
        let long = "x".repeat(100);
        let msg = AgentMessage::user(&long);
        let title = extract_title(&msg);
        assert!(title.ends_with('…'));
        assert!(title.chars().count() <= 82); // 80 + ellipsis
    }

    #[test]
    fn extract_title_short_text_unchanged() {
        let msg = AgentMessage::user("hi");
        let title = extract_title(&msg);
        assert_eq!(title, "hi");
    }

    fn uuid_v4() -> String {
        // Avoids depending on the uuid crate — use timestamp + counter for uniqueness.
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        format!(
            "{}-{}",
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos(),
            COUNTER.fetch_add(1, Ordering::SeqCst)
        )
    }
}

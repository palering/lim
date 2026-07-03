use camino::Utf8Path;
use lim_core::error::Result;
use lim_core::event::EventEnvelope;
use tokio::io::AsyncWriteExt;

/// Writes event envelopes to a JSONL file (one JSON object per line).
///
/// Each call to `write_event` serializes the envelope as a single line
/// and flushes to disk, ensuring durability for crash recovery.
pub struct SessionWriter {
    file: tokio::fs::File,
}

impl SessionWriter {
    /// Create a new session writer that truncates the given file on open.
    ///
    /// If the file does not exist, it will be created. If it exists, its
    /// previous contents are discarded — each session writes a fresh,
    /// self-contained JSONL stream.
    ///
    /// This is the correct default for lim's session model: `run_continue`
    /// re-emits all prior messages and `ContextEdited` events into the new
    /// file, so the new JSONL is fully self-contained without needing to
    /// append to an existing file. Append mode would silently pollute the
    /// new session with stale events from a previous run that happened to
    /// use the same output path (see `e2e-t1-responses.jsonl` incident
    /// where seq 1–4 from a failed run leaked into the next run).
    pub async fn create(path: &Utf8Path) -> Result<Self> {
        let file = tokio::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(path.as_std_path())
            .await
            .map_err(|e| {
                lim_core::error::LimError::tool_execution(
                    "session_writer",
                    format!("failed to open session file `{path}`: {e}"),
                    e,
                )
            })?;
        Ok(Self { file })
    }

    /// Write a single event envelope as a JSON line.
    pub async fn write_event(&mut self, envelope: &EventEnvelope) -> Result<()> {
        let mut line = serde_json::to_string(envelope).map_err(|e| {
            lim_core::error::LimError::tool_execution(
                "session_writer",
                format!("failed to serialize event: {e}"),
                e,
            )
        })?;
        line.push('\n');
        self.file.write_all(line.as_bytes()).await.map_err(|e| {
            lim_core::error::LimError::tool_execution(
                "session_writer",
                format!("failed to write event: {e}"),
                e,
            )
        })?;
        self.file.flush().await.map_err(|e| {
            lim_core::error::LimError::tool_execution(
                "session_writer",
                format!("failed to flush event: {e}"),
                e,
            )
        })?;
        Ok(())
    }

    /// Write a batch of events.
    pub async fn write_events(&mut self, events: &[EventEnvelope]) -> Result<()> {
        for event in events {
            self.write_event(event).await?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use camino::Utf8PathBuf;
    use lim_core::event::{AgentEvent, EventEnvelope};
    use lim_core::id::{BranchId, SessionId, TraceId};

    #[tokio::test]
    async fn write_single_event() {
        let dir = std::env::temp_dir().join("lim-test-session-writer");
        let _ = std::fs::create_dir_all(&dir);
        let path = Utf8PathBuf::try_from(dir.join("test.jsonl")).unwrap();

        let mut writer = SessionWriter::create(&path).await.unwrap();
        let envelope = EventEnvelope::new(
            1,
            SessionId::new(),
            BranchId::new(),
            TraceId::new(),
            AgentEvent::AgentStart,
        );
        writer.write_event(&envelope).await.unwrap();
        drop(writer);

        let content = std::fs::read_to_string(path.as_std_path()).unwrap();
        assert!(content.contains("\"event\":\"agent_start\""));
        assert!(content.ends_with('\n'));

        // Cleanup
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn write_multiple_events() {
        let dir = std::env::temp_dir().join("lim-test-session-writer-multi");
        let _ = std::fs::create_dir_all(&dir);
        let path = Utf8PathBuf::try_from(dir.join("multi.jsonl")).unwrap();

        let mut writer = SessionWriter::create(&path).await.unwrap();
        let events: Vec<EventEnvelope> = (0..3)
            .map(|i| {
                EventEnvelope::new(
                    i,
                    SessionId::new(),
                    BranchId::new(),
                    TraceId::new(),
                    AgentEvent::AgentStart,
                )
            })
            .collect();
        writer.write_events(&events).await.unwrap();
        drop(writer);

        let content = std::fs::read_to_string(path.as_std_path()).unwrap();
        let lines: Vec<&str> = content.trim().split('\n').collect();
        assert_eq!(lines.len(), 3);

        // Cleanup
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Regression: reusing the same output path must NOT pollute the new
    /// session with events from a previous run. The writer should truncate
    /// the file on open, so only the second run's events remain.
    #[tokio::test]
    async fn truncate_on_reuse_does_not_pollute() {
        let dir = std::env::temp_dir().join("lim-test-session-writer-truncate");
        let _ = std::fs::create_dir_all(&dir);
        let path = Utf8PathBuf::try_from(dir.join("reuse.jsonl")).unwrap();

        // First run: write 4 events with a marker session id.
        let session_a = SessionId::new();
        {
            let mut writer = SessionWriter::create(&path).await.unwrap();
            for seq in 1..=4u64 {
                writer
                    .write_event(&EventEnvelope::new(
                        seq,
                        session_a,
                        BranchId::new(),
                        TraceId::new(),
                        AgentEvent::AgentStart,
                    ))
                    .await
                    .unwrap();
            }
        }

        // Sanity: file has 4 lines from run A.
        let content_a = std::fs::read_to_string(path.as_std_path()).unwrap();
        assert_eq!(content_a.trim().split('\n').count(), 4);

        // Second run: reuse the same path, write 2 fresh events.
        let session_b = SessionId::new();
        {
            let mut writer = SessionWriter::create(&path).await.unwrap();
            for seq in 1..=2u64 {
                writer
                    .write_event(&EventEnvelope::new(
                        seq,
                        session_b,
                        BranchId::new(),
                        TraceId::new(),
                        AgentEvent::AgentStart,
                    ))
                    .await
                    .unwrap();
            }
        }

        // The file should now contain ONLY run B's 2 events — run A's
        // 4 events must have been truncated.
        let content_b = std::fs::read_to_string(path.as_std_path()).unwrap();
        let lines: Vec<&str> = content_b.trim().split('\n').collect();
        assert_eq!(lines.len(), 2, "truncated file must hold only run B's events");
        let session_a_str = session_a.to_string();
        let session_b_str = session_b.to_string();
        assert!(
            !content_b.contains(&session_a_str),
            "run A's session id must not leak into run B's file"
        );
        assert!(content_b.contains(&session_b_str), "run B's session id must be present");

        // Cleanup
        let _ = std::fs::remove_dir_all(dir);
    }
}

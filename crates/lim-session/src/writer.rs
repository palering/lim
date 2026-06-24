use camino::Utf8Path;
use lim_core::event::EventEnvelope;
use lim_core::error::Result;
use tokio::io::AsyncWriteExt;

/// Writes event envelopes to a JSONL file (one JSON object per line).
///
/// Each call to `write_event` serializes the envelope as a single line
/// and flushes to disk, ensuring durability for crash recovery.
pub struct SessionWriter {
    file: tokio::fs::File,
}

impl SessionWriter {
    /// Create a new session writer that appends to the given file.
    ///
    /// If the file does not exist, it will be created.
    pub async fn create(path: &Utf8Path) -> Result<Self> {
        let file = tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path.as_std_path())
            .await
            .map_err(|e| lim_core::error::LimError::ToolExecution {
                name: "session_writer".to_string(),
                message: format!("failed to open session file `{path}`: {e}"),
            })?;
        Ok(Self { file })
    }

    /// Write a single event envelope as a JSON line.
    pub async fn write_event(&mut self, envelope: &EventEnvelope) -> Result<()> {
        let mut line = serde_json::to_string(envelope).map_err(|e| {
            lim_core::error::LimError::ToolExecution {
                name: "session_writer".to_string(),
                message: format!("failed to serialize event: {e}"),
            }
        })?;
        line.push('\n');
        self.file.write_all(line.as_bytes()).await.map_err(|e| {
            lim_core::error::LimError::ToolExecution {
                name: "session_writer".to_string(),
                message: format!("failed to write event: {e}"),
            }
        })?;
        self.file.flush().await.map_err(|e| {
            lim_core::error::LimError::ToolExecution {
                name: "session_writer".to_string(),
                message: format!("failed to flush event: {e}"),
            }
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
}

use camino::Utf8Path;
use lim_core::event::EventEnvelope;
use lim_core::error::{LimError, Result};

/// Reads event envelopes from a JSONL file.
pub struct SessionReader;

impl SessionReader {
    /// Read all event envelopes from a JSONL file.
    ///
    /// Each line is parsed as a JSON-serialized `EventEnvelope`.
    /// Blank lines are skipped.
    pub async fn read_all(path: &Utf8Path) -> Result<Vec<EventEnvelope>> {
        let content = tokio::fs::read_to_string(path.as_std_path())
            .await
            .map_err(|e| LimError::ToolExecution {
                name: "session_reader".to_string(),
                message: format!("failed to read session file `{path}`: {e}"),
            })?;

        let mut events = Vec::new();
        for (line_num, line) in content.lines().enumerate() {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            let mut envelope: EventEnvelope =
                serde_json::from_str(trimmed).map_err(|e| LimError::ToolExecution {
                    name: "session_reader".to_string(),
                    message: format!("failed to parse line {}: {e}", line_num + 1),
                })?;
            envelope.recompute_ids();
            events.push(envelope);
        }

        Ok(events)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use camino::Utf8PathBuf;
    use lim_core::event::{AgentEvent, EventEnvelope};
    use lim_core::id::{BranchId, SessionId, TraceId};

    #[tokio::test]
    async fn read_empty_file() {
        let dir = std::env::temp_dir().join("lim-test-session-reader-empty");
        let _ = std::fs::create_dir_all(&dir);
        let path = Utf8PathBuf::try_from(dir.join("empty.jsonl")).unwrap();
        std::fs::write(path.as_std_path(), "").unwrap();

        let events = SessionReader::read_all(&path).await.unwrap();
        assert!(events.is_empty());

        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn read_written_events() {
        let dir = std::env::temp_dir().join("lim-test-session-reader");
        let _ = std::fs::create_dir_all(&dir);
        let path = Utf8PathBuf::try_from(dir.join("read.jsonl")).unwrap();

        // Write some events
        let mut writer = crate::writer::SessionWriter::create(&path).await.unwrap();
        let session_id = SessionId::new();
        let branch_id = BranchId::new();
        let trace_id = TraceId::new();
        for i in 0..3 {
            let envelope = EventEnvelope::new(
                i,
                session_id,
                branch_id,
                trace_id,
                AgentEvent::AgentStart,
            );
            writer.write_event(&envelope).await.unwrap();
        }
        drop(writer);

        // Read them back
        let events = SessionReader::read_all(&path).await.unwrap();
        assert_eq!(events.len(), 3);
        assert_eq!(events[0].seq, 0);
        assert_eq!(events[1].seq, 1);
        assert_eq!(events[2].seq, 2);

        let _ = std::fs::remove_dir_all(dir);
    }
}

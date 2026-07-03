use camino::Utf8Path;
use lim_core::error::{LimError, Result};
use lim_core::event::{AgentEvent, EventEnvelope};
use tracing::debug;

/// Reads event envelopes from a JSONL file.
pub struct SessionReader;

impl SessionReader {
    /// Read all event envelopes from a JSONL file.
    ///
    /// Each line is parsed as a JSON-serialized `EventEnvelope`.
    /// Blank lines are skipped.
    ///
    /// ## Schema version check
    ///
    /// After parsing, scans for the first `SessionHeader` event to
    /// determine the file's schema version:
    ///
    /// - **No header** (pre-`SessionHeader` files): treated as
    ///   `schema_version = 0` (legacy). `#[serde(default)]` on later-added
    ///   fields handles deserialization. A `debug!` log records this.
    /// - **Header schema_version == current**: normal read.
    /// - **Header schema_version < current**: read via compatibility path
    ///   (currently a no-op; future migrations hook in here). `debug!` log.
    /// - **Header schema_version > current**: the file was written by a
    ///   newer lim. Returns an error instructing the user to upgrade.
    ///
    /// ## Backward compatibility
    ///
    /// Old session files (pre-correlation-ID-refactor) serialized
    /// `turn_id`/`message_id`/`tool_call_id` at both the envelope level
    /// AND inside the flattened event, producing JSON objects with
    /// duplicate keys. Deserializing directly into `EventEnvelope` fails
    /// with "duplicate field" because serde's struct deserializer rejects
    /// duplicate keys.
    ///
    /// To support old files, we parse each line as a `serde_json::Value`
    /// first. `serde_json`'s Value parser silently deduplicates keys
    /// (last wins), so the resulting Value has each key only once. The
    /// current `EventEnvelope` marks the envelope-level correlation IDs
    /// as `#[serde(skip)]` and recomputes them from the event payload
    /// via `recompute_ids()`, so only the flattened event claims these
    /// fields — no conflict.
    pub async fn read_all(path: &Utf8Path) -> Result<Vec<EventEnvelope>> {
        let content = tokio::fs::read_to_string(path.as_std_path()).await.map_err(|e| {
            LimError::tool_execution(
                "session_reader",
                format!("failed to read session file `{path}`: {e}"),
                e,
            )
        })?;

        let mut events = Vec::new();
        for (line_num, line) in content.lines().enumerate() {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }

            // Parse as a generic JSON value first. serde_json's Value
            // parser deduplicates duplicate keys (last wins), which lets
            // us handle old session files that have the same field at
            // both the envelope and event level.
            let value: serde_json::Value = serde_json::from_str(trimmed).map_err(|e| {
                LimError::tool_execution(
                    "session_reader",
                    format!("failed to parse line {}: {e}", line_num + 1),
                    e,
                )
            })?;

            let mut envelope: EventEnvelope = serde_json::from_value(value).map_err(|e| {
                LimError::tool_execution(
                    "session_reader",
                    format!("failed to parse line {}: {e}", line_num + 1),
                    e,
                )
            })?;
            envelope.recompute_ids();
            events.push(envelope);
        }

        Self::check_schema_version(path, &events)?;

        Ok(events)
    }

    /// Check the schema version of a parsed event list.
    ///
    /// See [`read_all`](Self::read_all) for the version policy.
    fn check_schema_version(path: &Utf8Path, events: &[EventEnvelope]) -> Result<()> {
        let file_schema = events.iter().find_map(|e| match &e.event {
            AgentEvent::SessionHeader { schema_version, .. } => Some(*schema_version),
            _ => None,
        });

        match file_schema {
            None => {
                debug!(
                    path = %path,
                    "session file has no SessionHeader; treating as schema_version=0 (legacy)"
                );
            }
            Some(v) if v == lim_core::SCHEMA_VERSION => {
                debug!(
                    path = %path,
                    schema_version = v,
                    "session file schema version matches current"
                );
            }
            Some(v) if v < lim_core::SCHEMA_VERSION => {
                debug!(
                    path = %path,
                    file_schema = v,
                    current_schema = lim_core::SCHEMA_VERSION,
                    "session file uses older schema; reading via compatibility path"
                );
                // Future: invoke migrate(events, v) here when a breaking
                // schema change is introduced. See
                // docs/architecture/behavior-trace.md "Future migration plan".
            }
            Some(v) => {
                // v > current — file written by a newer lim; refuse to read.
                return Err(LimError::tool_execution_msg(
                    "session_reader",
                    format!(
                        "session file `{path}` was written with schema_version {v}, \
                         but this lim supports up to schema_version {}. \
                         Please upgrade lim to read this file.",
                        lim_core::SCHEMA_VERSION
                    ),
                ));
            }
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
            let envelope =
                EventEnvelope::new(i, session_id, branch_id, trace_id, AgentEvent::AgentStart);
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

    /// Old session files (pre-correlation-ID-refactor) serialized
    /// `turn_id`/`message_id`/`tool_call_id` at both the envelope level
    /// and inside the flattened event, producing JSON with duplicate keys.
    /// The Value-first parse deduplicates (last wins), allowing these
    /// files to be read.
    #[tokio::test]
    async fn read_legacy_duplicate_keys() {
        let dir = std::env::temp_dir().join("lim-test-session-reader-legacy-dup");
        let _ = std::fs::create_dir_all(&dir);
        let path = Utf8PathBuf::try_from(dir.join("legacy_dup.jsonl")).unwrap();

        // Note: turn_id appears TWICE in this line — once at the envelope
        // level (before trace_id) and once inside the event (after event).
        // This mirrors the old serialization format.
        let line = r#"{"seq":1,"session_id":"01KVWD0DAWV9BYM6XS736PV0ZR","branch_id":"01KVWD0DAWXG0FRBYTNH6R30P6","turn_id":"01KVWD0DAWDY95KYQMPACR7BM6","trace_id":"01KVWD0DAW8B7YBY61AHSKFBHA","event":"turn_start","turn_id":"01KVWD0DAWDY95KYQMPACR7BM6"}"#;
        std::fs::write(path.as_std_path(), line).unwrap();

        let events = SessionReader::read_all(&path).await.unwrap();
        assert_eq!(events.len(), 1);

        // The turn_id should be correctly extracted from the event.
        match &events[0].event {
            AgentEvent::TurnStart { turn_id, .. } => {
                assert_eq!(turn_id.to_string(), "01KVWD0DAWDY95KYQMPACR7BM6");
            }
            other => panic!("expected TurnStart, got {other:?}"),
        }
        // recompute_ids should have populated the envelope-level turn_id.
        assert!(events[0].turn_id.is_some());

        let _ = std::fs::remove_dir_all(dir);
    }

    /// Old session files predate the self-auditing TurnStart fields
    /// (model, message_count, tool_count, tool_names, system_prompt_len).
    /// `#[serde(default)]` on these fields lets old files deserialize
    /// with sensible defaults.
    #[tokio::test]
    async fn read_legacy_turn_start_missing_fields() {
        let dir = std::env::temp_dir().join("lim-test-session-reader-legacy-turnstart");
        let _ = std::fs::create_dir_all(&dir);
        let path = Utf8PathBuf::try_from(dir.join("legacy_turnstart.jsonl")).unwrap();

        // TurnStart with only turn_id — no model, tool_count, etc.
        let line = r#"{"seq":1,"session_id":"01KVWD0DAWV9BYM6XS736PV0ZR","branch_id":"01KVWD0DAWXG0FRBYTNH6R30P6","trace_id":"01KVWD0DAW8B7YBY61AHSKFBHA","event":"turn_start","turn_id":"01KVWD0DAWDY95KYQMPACR7BM6"}"#;
        std::fs::write(path.as_std_path(), line).unwrap();

        let events = SessionReader::read_all(&path).await.unwrap();
        assert_eq!(events.len(), 1);

        match &events[0].event {
            AgentEvent::TurnStart {
                turn_id,
                model,
                message_count,
                tool_count,
                tool_names,
                system_prompt_len,
            } => {
                assert_eq!(turn_id.to_string(), "01KVWD0DAWDY95KYQMPACR7BM6");
                assert_eq!(model, "");
                assert_eq!(*message_count, 0);
                assert_eq!(*tool_count, 0);
                assert!(tool_names.is_empty());
                assert_eq!(*system_prompt_len, None);
            }
            other => panic!("expected TurnStart, got {other:?}"),
        }

        let _ = std::fs::remove_dir_all(dir);
    }

    /// Old session files predate the ToolExecutionStart/End timestamp
    /// fields (started_at, ended_at). `#[serde(default)]` lets old files
    /// deserialize with default 0.
    #[tokio::test]
    async fn read_legacy_tool_execution_missing_timestamps() {
        let dir = std::env::temp_dir().join("lim-test-session-reader-legacy-timestamps");
        let _ = std::fs::create_dir_all(&dir);
        let path = Utf8PathBuf::try_from(dir.join("legacy_timestamps.jsonl")).unwrap();

        // Two lines: ToolExecutionStart without started_at, then
        // ToolExecutionEnd without ended_at.
        let content = concat!(
            r#"{"seq":1,"session_id":"01KVWD0DAWV9BYM6XS736PV0ZR","branch_id":"01KVWD0DAWXG0FRBYTNH6R30P6","trace_id":"01KVWD0DAW8B7YBY61AHSKFBHA","event":"tool_execution_start","tool_call_id":"01KVWD0DAWMSSMRRDC3GMXA08X","tool_name":"shell"}"#,
            "\n",
            r#"{"seq":2,"session_id":"01KVWD0DAWV9BYM6XS736PV0ZR","branch_id":"01KVWD0DAWXG0FRBYTNH6R30P6","trace_id":"01KVWD0DAW8B7YBY61AHSKFBHA","event":"tool_execution_end","tool_call_id":"01KVWD0DAWMSSMRRDC3GMXA08X","result":{"tool_call_id":"01KVWD0DAWMSSMRRDC3GMXA08X","output":"done","is_error":false}}"#,
        );
        std::fs::write(path.as_std_path(), content).unwrap();

        let events = SessionReader::read_all(&path).await.unwrap();
        assert_eq!(events.len(), 2);

        match &events[0].event {
            AgentEvent::ToolExecutionStart { tool_call_id, tool_name, started_at } => {
                assert_eq!(tool_call_id.as_str(), "01KVWD0DAWMSSMRRDC3GMXA08X");
                assert_eq!(tool_name, "shell");
                assert_eq!(*started_at, 0); // default
            }
            other => panic!("expected ToolExecutionStart, got {other:?}"),
        }

        match &events[1].event {
            AgentEvent::ToolExecutionEnd { tool_call_id, result, ended_at } => {
                assert_eq!(tool_call_id.as_str(), "01KVWD0DAWMSSMRRDC3GMXA08X");
                assert_eq!(result.output, "done");
                assert!(!result.is_error);
                assert_eq!(*ended_at, 0); // default
            }
            other => panic!("expected ToolExecutionEnd, got {other:?}"),
        }

        let _ = std::fs::remove_dir_all(dir);
    }

    /// A file with a `SessionHeader` at the current schema version reads
    /// successfully, and the header is the first event.
    #[tokio::test]
    async fn read_file_with_current_schema_header() {
        let dir = std::env::temp_dir().join("lim-test-session-reader-header-current");
        let _ = std::fs::create_dir_all(&dir);
        let path = Utf8PathBuf::try_from(dir.join("header_current.jsonl")).unwrap();

        let session_id = SessionId::new();
        let branch_id = BranchId::new();
        let trace_id = TraceId::new();

        let mut writer = crate::writer::SessionWriter::create(&path).await.unwrap();
        writer
            .write_event(&EventEnvelope::new(
                1,
                session_id,
                branch_id,
                trace_id,
                AgentEvent::SessionHeader {
                    lim_version: lim_core::LIM_VERSION.to_string(),
                    schema_version: lim_core::SCHEMA_VERSION,
                    platform: lim_core::platform_string(),
                },
            ))
            .await
            .unwrap();
        writer
            .write_event(&EventEnvelope::new(
                2,
                session_id,
                branch_id,
                trace_id,
                AgentEvent::AgentStart,
            ))
            .await
            .unwrap();
        drop(writer);

        let events = SessionReader::read_all(&path).await.unwrap();
        assert_eq!(events.len(), 2);

        // First event must be SessionHeader.
        match &events[0].event {
            AgentEvent::SessionHeader { schema_version, lim_version, platform } => {
                assert_eq!(*schema_version, lim_core::SCHEMA_VERSION);
                assert_eq!(lim_version, lim_core::LIM_VERSION);
                assert!(!platform.is_empty());
            }
            other => panic!("expected SessionHeader, got {other:?}"),
        }
        match &events[1].event {
            AgentEvent::AgentStart => {}
            other => panic!("expected AgentStart, got {other:?}"),
        }

        let _ = std::fs::remove_dir_all(dir);
    }

    /// A file whose `SessionHeader` reports a *future* schema version
    /// (greater than `SCHEMA_VERSION`) must be refused with a clear error.
    #[tokio::test]
    async fn read_file_with_future_schema_version_is_rejected() {
        let dir = std::env::temp_dir().join("lim-test-session-reader-header-future");
        let _ = std::fs::create_dir_all(&dir);
        let path = Utf8PathBuf::try_from(dir.join("header_future.jsonl")).unwrap();

        let future_version = lim_core::SCHEMA_VERSION + 1;
        let line = format!(
            r#"{{"seq":1,"session_id":"01KVWD0DAWV9BYM6XS736PV0ZR","branch_id":"01KVWD0DAWXG0FRBYTNH6R30P6","trace_id":"01KVWD0DAW8B7YBY61AHSKFBHA","event":"session_header","lim_version":"9.9.9","schema_version":{future_version},"platform":"test/unknown"}}"#
        );
        std::fs::write(path.as_std_path(), line).unwrap();

        let result = SessionReader::read_all(&path).await;
        let err = result.expect_err("future schema version must be rejected");
        let msg = err.to_string();
        assert!(
            msg.contains(&future_version.to_string()),
            "error message should mention the file's schema version: {msg}"
        );
        assert!(
            msg.contains("upgrade"),
            "error message should instruct the user to upgrade: {msg}"
        );

        let _ = std::fs::remove_dir_all(dir);
    }

    /// A file whose `SessionHeader` reports an *older* schema version
    /// (less than `SCHEMA_VERSION`) is read successfully via the
    /// compatibility path. (Currently a no-op; migrations hook in here.)
    #[tokio::test]
    async fn read_file_with_older_schema_version_uses_compat_path() {
        let dir = std::env::temp_dir().join("lim-test-session-reader-header-older");
        let _ = std::fs::create_dir_all(&dir);
        let path = Utf8PathBuf::try_from(dir.join("header_older.jsonl")).unwrap();

        // Use schema_version 0 (legacy) explicitly in the header.
        let line = r#"{"seq":1,"session_id":"01KVWD0DAWV9BYM6XS736PV0ZR","branch_id":"01KVWD0DAWXG0FRBYTNH6R30P6","trace_id":"01KVWD0DAW8B7YBY61AHSKFBHA","event":"session_header","lim_version":"0.0.1","schema_version":0,"platform":"test/unknown"}"#;
        std::fs::write(path.as_std_path(), line).unwrap();

        // Should read successfully — older versions take the compat path.
        let events = SessionReader::read_all(&path).await.unwrap();
        assert_eq!(events.len(), 1);

        let _ = std::fs::remove_dir_all(dir);
    }
}

use lim_core::content::ContentBlock;
use lim_core::id::ToolCallId;
use lim_core::message::{AgentMessage, Role};
use lim_core::stop_reason::StopReason;
use lim_core::usage::Usage;

use lim_llm::stream::StreamEvent;

/// Accumulates streaming events into a complete [`AgentMessage`].
///
/// Handles incremental text, thinking, and tool-call construction from
/// a stream of [`StreamEvent`]s.
pub struct StreamAccumulator {
    /// Current text being accumulated for the assistant response.
    text: String,
    /// Current thinking content being accumulated.
    thinking: String,
    /// Tool calls being assembled (id -> (name, accumulated JSON args)).
    tool_calls: Vec<(ToolCallId, String, String)>,
    /// Final usage stats from the response.
    usage: Option<Usage>,
    /// Stop reason from the Done event.
    stop_reason: Option<StopReason>,
    /// Message ID assigned when we first see content.
    message_id: Option<lim_core::MessageId>,
}

impl StreamAccumulator {
    /// Create an empty [`StreamAccumulator`].
    pub fn new() -> Self {
        Self {
            text: String::new(),
            thinking: String::new(),
            tool_calls: Vec::new(),
            usage: None,
            stop_reason: None,
            message_id: None,
        }
    }

    /// The message ID for this accumulator's message.
    /// Creates one on first access.
    pub fn message_id(&mut self) -> lim_core::MessageId {
        *self.message_id.get_or_insert_with(lim_core::MessageId::new)
    }

    /// Reset content state while preserving the message ID.
    ///
    /// Used when retrying a stream after a retryable error: the accumulator
    /// keeps its assigned `message_id` (so the TUI's streaming view stays
    /// stable), but discards any partial content accumulated from the failed
    /// attempt.
    pub fn reset(&mut self) {
        self.text.clear();
        self.thinking.clear();
        self.tool_calls.clear();
        self.usage = None;
        self.stop_reason = None;
    }

    /// Process a single stream event, returning the delta string for event emission.
    ///
    /// Returns `Some(delta_text)` if this event produced visible content,
    /// `None` for metadata events like Usage or Done.
    pub fn ingest(&mut self, event: StreamEvent) -> Option<String> {
        match event {
            StreamEvent::TextDelta { text } => {
                self.text.push_str(&text);
                Some(text)
            }
            StreamEvent::ThinkingDelta { text } => {
                self.thinking.push_str(&text);
                None // thinking deltas don't produce visible MessageUpdate events
            }
            StreamEvent::ToolCallStart { id, name } => {
                // If we already have an entry for this id (e.g. name filled in later),
                // update the name instead of creating a duplicate.
                if let Some(entry) = self.tool_calls.iter_mut().find(|(tid, _, _)| *tid == id) {
                    if entry.1.is_empty()
                        && let Some(n) = name
                    {
                        entry.1 = n;
                    }
                } else {
                    self.tool_calls.push((id, name.unwrap_or_default(), String::new()));
                }
                None
            }
            StreamEvent::ToolCallDelta { id, arguments_delta } => {
                // Find the matching tool call and append the delta
                if let Some(entry) = self.tool_calls.iter_mut().find(|(tid, _, _)| *tid == id) {
                    entry.2.push_str(&arguments_delta);
                }
                None
            }
            StreamEvent::Usage(usage) => {
                self.usage = Some(usage);
                None
            }
            StreamEvent::Done { stop_reason } => {
                self.stop_reason = Some(stop_reason);
                None
            }
        }
    }

    /// Build the final [`AgentMessage`] from accumulated data.
    pub fn into_message(self) -> (AgentMessage, Option<Usage>, Option<StopReason>) {
        let mut content: Vec<ContentBlock> = Vec::new();

        if !self.thinking.is_empty() {
            content.push(ContentBlock::Thinking { text: self.thinking });
        }

        if !self.text.is_empty() {
            content.push(ContentBlock::Text { text: self.text });
        }

        for (id, name, args_str) in self.tool_calls {
            let arguments = match serde_json::from_str(&args_str) {
                Ok(v) => v,
                Err(e) => {
                    // LLM returned malformed JSON arguments for this tool call.
                    // We fall back to wrapping the raw string as a Value::String
                    // so the tool still receives *something*, but log a warning
                    // so silent degradation is observable.
                    tracing::warn!(
                        tool_call_id = %id,
                        tool_name = %name,
                        args_len = args_str.len(),
                        error = %e,
                        "tool call arguments JSON parse failed, falling back to raw string"
                    );
                    serde_json::Value::String(args_str)
                }
            };
            content.push(ContentBlock::ToolCall { id, name, arguments });
        }

        let msg_id = self.message_id.unwrap_or_default();

        let message = AgentMessage {
            id: msg_id,
            role: Role::Assistant,
            content,
            tool_call_ids: Vec::new(),
            is_error: false,
        };

        (message, self.usage, self.stop_reason)
    }

    /// Get current usage if available.
    pub fn usage(&self) -> Option<&Usage> {
        self.usage.as_ref()
    }

    /// Get current stop reason if available.
    pub fn stop_reason(&self) -> Option<StopReason> {
        self.stop_reason
    }
}

impl Default for StreamAccumulator {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accumulate_text_only() {
        let mut acc = StreamAccumulator::new();
        acc.ingest(StreamEvent::TextDelta { text: "Hello ".to_string() });
        acc.ingest(StreamEvent::TextDelta { text: "world!".to_string() });
        acc.ingest(StreamEvent::Usage(Usage::new(10, 20)));
        acc.ingest(StreamEvent::Done { stop_reason: StopReason::EndTurn });

        let (msg, usage, reason) = acc.into_message();
        assert_eq!(msg.content.len(), 1);
        match &msg.content[0] {
            ContentBlock::Text { text } => assert_eq!(text, "Hello world!"),
            _ => panic!("expected text block"),
        }
        assert!(usage.is_some());
        assert_eq!(reason, Some(StopReason::EndTurn));
    }

    #[test]
    fn accumulate_tool_call() {
        let tc_id = ToolCallId::new();
        let mut acc = StreamAccumulator::new();
        acc.ingest(StreamEvent::TextDelta { text: "Let me check".to_string() });
        acc.ingest(StreamEvent::ToolCallStart {
            id: tc_id.clone(),
            name: Some("bash".to_string()),
        });
        acc.ingest(StreamEvent::ToolCallDelta {
            id: tc_id.clone(),
            arguments_delta: "{\"cmd\":\"ls\"}".to_string(),
        });
        acc.ingest(StreamEvent::Done { stop_reason: StopReason::ToolCall });

        let (msg, _, reason) = acc.into_message();
        assert_eq!(msg.content.len(), 2);
        assert!(msg.has_tool_calls());
        assert_eq!(reason, Some(StopReason::ToolCall));

        let calls = msg.tool_calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, tc_id);
        assert_eq!(calls[0].1, "bash");
    }
}

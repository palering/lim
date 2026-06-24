use serde::{Deserialize, Serialize};

use crate::id::{BranchId, MessageId, SessionId, ToolCallId, TraceId, TurnId};
use crate::message::AgentMessage;
use crate::stop_reason::StopReason;
use crate::tool::ToolResult;
use crate::usage::Usage;

/// Events emitted by the agent runtime.
///
/// These are the core contract between the agent loop and external consumers
/// (session logger, TUI, CLI, replay engine). Every event carries enough
/// context to be correlated across sessions, branches, and turns.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum AgentEvent {
    /// Agent run started.
    AgentStart,

    /// A new turn is beginning.
    TurnStart { turn_id: TurnId },

    /// A message is being produced (first chunk arrived).
    MessageStart {
        message_id: MessageId,
        role: crate::message::Role,
    },

    /// Streaming content update for a message (incremental).
    MessageUpdate {
        message_id: MessageId,
        /// Index into the content blocks vector.
        content_index: usize,
        /// Text delta (for text/thinking blocks) or partial JSON (for tool calls).
        delta: String,
    },

    /// A message is complete.
    MessageEnd {
        message_id: MessageId,
        /// The fully assembled message.
        message: AgentMessage,
    },

    /// A tool is about to be executed.
    ToolExecutionStart {
        tool_call_id: ToolCallId,
        tool_name: String,
    },

    /// Streaming output from a tool execution.
    ToolExecutionUpdate {
        tool_call_id: ToolCallId,
        output: String,
    },

    /// Tool execution completed.
    ToolExecutionEnd {
        tool_call_id: ToolCallId,
        result: ToolResult,
    },

    /// A turn has ended.
    TurnEnd {
        turn_id: TurnId,
        stop_reason: StopReason,
        usage: Usage,
    },

    /// Agent run finished.
    AgentEnd,

    /// Context was compacted to fit within the model's window.
    ContextCompacted {
        /// Number of messages before compaction.
        messages_before: usize,
        /// Number of messages after compaction (summary + recent).
        messages_after: usize,
        /// Estimated tokens before compaction.
        tokens_before: u64,
        /// Estimated tokens after compaction.
        tokens_after: u64,
    },

    /// Context was manually edited (user or loop hook).
    ///
    /// Persisted in the JSONL event log so that edits can be replayed
    /// on session restore. The edit describes modifications to the
    /// LLM projection only — the original message history is unchanged.
    ContextEdited {
        edit: crate::context_edit::ContextEdit,
    },
}

/// Envelope wrapping every event with correlation IDs.
///
/// This is what gets written to the JSONL event log.
///
/// Note: `turn_id`, `message_id`, and `tool_call_id` are not serialized
/// because they can conflict with the same fields inside the flattened
/// `AgentEvent`. They are recomputed from the event on deserialization
/// via `extract_ids`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventEnvelope {
    /// Monotonically increasing sequence number within the session.
    pub seq: u64,
    /// The session this event belongs to.
    pub session_id: SessionId,
    /// The branch within the session.
    pub branch_id: BranchId,
    /// Current turn (recomputed from event, not serialized).
    #[serde(skip, default)]
    pub turn_id: Option<TurnId>,
    /// Current message (recomputed from event, not serialized).
    #[serde(skip, default)]
    pub message_id: Option<MessageId>,
    /// Current tool call (recomputed from event, not serialized).
    #[serde(skip, default)]
    pub tool_call_id: Option<ToolCallId>,
    /// Trace ID for correlating across runs.
    pub trace_id: TraceId,
    /// The event payload.
    #[serde(flatten)]
    pub event: AgentEvent,
}

impl EventEnvelope {
    /// Create a new event envelope with the given correlation context.
    pub fn new(
        seq: u64,
        session_id: SessionId,
        branch_id: BranchId,
        trace_id: TraceId,
        event: AgentEvent,
    ) -> Self {
        let (turn_id, message_id, tool_call_id) = extract_ids(&event);
        Self {
            seq,
            session_id,
            branch_id,
            turn_id,
            message_id,
            tool_call_id,
            trace_id,
            event,
        }
    }

    /// Recompute correlation IDs from the event payload.
    ///
    /// Call this after deserialization, since `turn_id`, `message_id`,
    /// and `tool_call_id` are skipped during serde to avoid conflicts
    /// with the flattened event.
    pub fn recompute_ids(&mut self) {
        let (turn_id, message_id, tool_call_id) = extract_ids(&self.event);
        self.turn_id = turn_id;
        self.message_id = message_id;
        self.tool_call_id = tool_call_id;
    }
}

/// Extract correlation IDs from an event, if present.
fn extract_ids(event: &AgentEvent) -> (Option<TurnId>, Option<MessageId>, Option<ToolCallId>) {
    match event {
        AgentEvent::TurnStart { turn_id } => (Some(*turn_id), None, None),
        AgentEvent::TurnEnd { turn_id, .. } => (Some(*turn_id), None, None),
        AgentEvent::MessageStart { message_id, .. } => (None, Some(*message_id), None),
        AgentEvent::MessageUpdate { message_id, .. } => (None, Some(*message_id), None),
        AgentEvent::MessageEnd { message_id, .. } => (None, Some(*message_id), None),
        AgentEvent::ToolExecutionStart { tool_call_id, .. } => {
            (None, None, Some(*tool_call_id))
        }
        AgentEvent::ToolExecutionUpdate { tool_call_id, .. } => {
            (None, None, Some(*tool_call_id))
        }
        AgentEvent::ToolExecutionEnd {
            tool_call_id,
            result: _,
        } => (None, None, Some(*tool_call_id)),
        _ => (None, None, None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn envelope_serde() {
        let envelope = EventEnvelope::new(
            1,
            SessionId::new(),
            BranchId::new(),
            TraceId::new(),
            AgentEvent::AgentStart,
        );
        let json = serde_json::to_string(&envelope).unwrap();
        assert!(json.contains("\"event\":\"agent_start\""));
        let parsed: EventEnvelope = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.seq, 1);
    }

    #[test]
    fn extract_ids_turn_start() {
        let turn_id = TurnId::new();
        let event = AgentEvent::TurnStart { turn_id };
        let (tid, mid, tcid) = extract_ids(&event);
        assert_eq!(tid, Some(turn_id));
        assert!(mid.is_none());
        assert!(tcid.is_none());
    }

    #[test]
    fn extract_ids_tool_execution() {
        let tc_id = ToolCallId::new();
        let event = AgentEvent::ToolExecutionStart {
            tool_call_id: tc_id,
            tool_name: "bash".to_string(),
        };
        let (tid, mid, tcid) = extract_ids(&event);
        assert!(tid.is_none());
        assert!(mid.is_none());
        assert_eq!(tcid, Some(tc_id));
    }
}

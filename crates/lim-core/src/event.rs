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
    /// Session metadata header, emitted as the first event of every session.
    ///
    /// Captures the writer's lim version, schema version, and platform so
    /// that future readers can detect incompatible files and apply
    /// migrations. Old session files (pre-SessionHeader) have no such
    /// event and are treated as `schema_version = 0` (legacy) by the
    /// reader — `#[serde(default)]` on later-added fields already handles
    /// their deserialization.
    ///
    /// See [`crate::SCHEMA_VERSION`] for the current schema version and
    /// the "Future migration plan" section in
    /// `docs/architecture/behavior-trace.md` for the migration strategy.
    SessionHeader {
        /// lim crate version that wrote this session (e.g. "0.1.0").
        ///
        /// For debug/audit only — NOT used for compatibility decisions.
        /// Use `schema_version` for that.
        lim_version: String,
        /// Monotonically increasing integer bumped ONLY on breaking schema
        /// changes that `#[serde(default)]` cannot handle.
        ///
        /// Current version: [`crate::SCHEMA_VERSION`].
        schema_version: u32,
        /// Writer platform, formatted as `"{os}/{arch}"` (e.g.
        /// "macos/aarch64"). Useful for diagnosing path-separator or
        /// line-ending differences when replaying cross-platform sessions.
        platform: String,
    },

    /// Agent run started.
    AgentStart,

    /// A new turn is beginning.
    ///
    /// Carries a summary of the `LlmRequest` that will be sent to the provider
    /// so that the session JSONL is self-auditing: a reader can reconstruct
    /// what tools/system prompt were presented to the LLM without needing to
    /// read the source code.
    ///
    /// All fields except `turn_id` are `#[serde(default)]` for backward
    /// compatibility with old session files that predate the self-auditing
    /// fields (model, tool list, system prompt length).
    TurnStart {
        turn_id: TurnId,
        /// Model name requested (e.g. "gpt-4o").
        #[serde(default)]
        model: String,
        /// Number of messages in the request (including system if any).
        #[serde(default)]
        message_count: usize,
        /// Number of tools made available to the model.
        #[serde(default)]
        tool_count: usize,
        /// Sorted list of tool names made available to the model.
        ///
        /// Sorted so the persisted record is stable and reproducible.
        #[serde(default)]
        tool_names: Vec<String>,
        /// Length in bytes of the system prompt, if any.
        ///
        /// We persist only the *length*, not the content, to avoid leaking
        /// potentially sensitive prompt content into the JSONL. A reader
        /// who needs the actual prompt should look at the source config.
        #[serde(default)]
        system_prompt_len: Option<usize>,
    },

    /// A message is being produced (first chunk arrived).
    MessageStart { message_id: MessageId, role: crate::message::Role },

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
    ///
    /// `started_at` is a Unix millisecond timestamp so tool execution
    /// duration is observable by diffing with `ToolExecutionEnd::ended_at`.
    /// `started_at` is `#[serde(default)]` for backward compatibility with
    /// old session files that predate timestamp fields.
    ToolExecutionStart {
        tool_call_id: ToolCallId,
        tool_name: String,
        /// Unix ms timestamp when execution started.
        #[serde(default)]
        started_at: u64,
    },

    /// Streaming output from a tool execution.
    ToolExecutionUpdate { tool_call_id: ToolCallId, output: String },

    /// Tool execution completed.
    ///
    /// `ended_at` is a Unix millisecond timestamp so duration can be
    /// computed as `ended_at - ToolExecutionStart::started_at`.
    /// `ended_at` is `#[serde(default)]` for backward compatibility with
    /// old session files that predate timestamp fields.
    ToolExecutionEnd {
        tool_call_id: ToolCallId,
        result: ToolResult,
        /// Unix ms timestamp when execution finished.
        #[serde(default)]
        ended_at: u64,
    },

    /// A turn has ended.
    TurnEnd { turn_id: TurnId, stop_reason: StopReason, usage: Usage },

    /// Agent run finished.
    AgentEnd,

    /// A session ended early (before `AgentEnd`) due to a Loop Engineering
    /// stop decision. Emitted when `AgentHooks::should_stop` returns
    /// `StopSession`, or when `max_steps` is reached. Carries the reason
    /// so the `LoopRunner` (Goal Loop) can decide whether to start a new
    /// session.
    ///
    /// See `docs/architecture/loop-engineering.md` §3.
    SessionEnd {
        /// Why the session ended.
        reason: crate::loop_end::SessionEndReason,
    },

    /// Context was compacted to fit within the model's window.
    ///
    /// Compaction triggers a hidden LLM call (summarization). The metadata
    /// of that call (model + token usage) is captured here so the JSONL
    /// reader can audit it without reading the source. See
    /// docs/architecture/behavior-trace.md §8.1.
    ContextCompacted {
        /// Number of messages before compaction.
        messages_before: usize,
        /// Number of messages after compaction (summary + recent).
        messages_after: usize,
        /// Estimated tokens before compaction.
        tokens_before: u64,
        /// Estimated tokens after compaction.
        tokens_after: u64,
        /// Model used for the hidden summarization LLM call.
        summary_model: String,
        /// Prompt tokens reported by the provider for the summarization call.
        /// 0 if the provider did not report usage (e.g. stream errored).
        summary_prompt_tokens: u64,
        /// Completion tokens reported by the provider for the summarization call.
        summary_completion_tokens: u64,
    },

    /// Context was manually edited (user or loop hook).
    ///
    /// Persisted in the JSONL event log so that edits can be replayed
    /// on session restore. The edit describes modifications to the
    /// LLM projection only — the original message history is unchanged.
    ContextEdited { edit: crate::context_edit::ContextEdit },

    /// A recoverable error occurred during the agent loop.
    ///
    /// Emitted when a provider stream error is classified as retryable
    /// (rate limit, transport, server overload) and the agent is about to
    /// retry the request, OR when retries are exhausted and the turn is
    /// about to fail. Persisted to the session JSONL so error history is
    /// preserved across resumes.
    Error {
        /// Human-readable error message.
        message: String,
        /// Whether the agent loop considers this error retryable.
        retryable: bool,
        /// Retry attempt number (1-based). 0 means no retry attempted.
        retry_count: u32,
        /// Max retries the agent loop will attempt before giving up.
        max_retries: u32,
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
    /// Agent identifier — distinguishes events from different agents in
    /// multi-agent scenarios.
    ///
    /// - `None` or `"main"`: the primary agent in the session
    /// - `"reviewer"` / `"planner"` / `"coder"`: sub-agent role
    /// - custom string: any agent ID
    ///
    /// `#[serde(default)]` keeps backward compat with old session JSONL
    /// files written before multi-agent support (they are treated as "main").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
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
            agent_id: None,
            trace_id,
            event,
        }
    }

    /// Create a new event envelope with an explicit agent ID.
    pub fn with_agent_id(
        seq: u64,
        session_id: SessionId,
        branch_id: BranchId,
        trace_id: TraceId,
        agent_id: impl Into<String>,
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
            agent_id: Some(agent_id.into()),
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
        AgentEvent::TurnStart { turn_id, .. } => (Some(*turn_id), None, None),
        AgentEvent::TurnEnd { turn_id, .. } => (Some(*turn_id), None, None),
        AgentEvent::MessageStart { message_id, .. } => (None, Some(*message_id), None),
        AgentEvent::MessageUpdate { message_id, .. } => (None, Some(*message_id), None),
        AgentEvent::MessageEnd { message_id, .. } => (None, Some(*message_id), None),
        AgentEvent::ToolExecutionStart { tool_call_id, .. } => {
            (None, None, Some(tool_call_id.clone()))
        }
        AgentEvent::ToolExecutionUpdate { tool_call_id, .. } => {
            (None, None, Some(tool_call_id.clone()))
        }
        AgentEvent::ToolExecutionEnd { tool_call_id, .. } => {
            (None, None, Some(tool_call_id.clone()))
        }
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
        let event = AgentEvent::TurnStart {
            turn_id,
            model: "gpt-4o".to_string(),
            message_count: 1,
            tool_count: 0,
            tool_names: vec![],
            system_prompt_len: None,
        };
        let (tid, mid, tcid) = extract_ids(&event);
        assert_eq!(tid, Some(turn_id));
        assert!(mid.is_none());
        assert!(tcid.is_none());
    }

    #[test]
    fn extract_ids_tool_execution() {
        let tc_id = ToolCallId::new();
        let event = AgentEvent::ToolExecutionStart {
            tool_call_id: tc_id.clone(),
            tool_name: "bash".to_string(),
            started_at: 0,
        };
        let (tid, mid, tcid) = extract_ids(&event);
        assert!(tid.is_none());
        assert!(mid.is_none());
        assert_eq!(tcid, Some(tc_id));
    }
}

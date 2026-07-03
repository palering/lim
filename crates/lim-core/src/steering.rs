//! Steering message — a soft intervention signal injected into the agent
//! loop while it is running.
//!
//! ## Concept
//!
//! Steering is distinct from:
//!
//! - **Cancel** (`CancellationToken`): a hard interrupt that stops the
//!   agent immediately with transactional rollback (drops temp files,
//!   SIGTERM bash). The session ends.
//! - **Follow-up**: a message queued while the agent is idle, appended
//!   to the next turn naturally.
//! - **Hooks** (`AgentHooks`): passive decision points called by the
//!   AgentLoop at fixed moments (before/after tool, turn end). Hooks
//!   return decisions, not user messages.
//!
//! Steering sits between cancel and follow-up: the user wants to
//! **redirect** the running agent without stopping it. The message is
//! queued via an mpsc channel and drained at the next turn boundary
//! (before the next LLM call), so the agent sees it as a new User
//! message appended to the conversation.
//!
//! ## When it takes effect
//!
//! P0 (current): at the **turn boundary**. If the agent is mid-LLM-stream
//! or mid-tool-execution, the steering message waits until the current
//! turn finishes, then is injected before the next turn starts.
//!
//! P1 (future, optional): at the **tool boundary** — check the steering
//! queue before each tool call in a multi-tool turn, skip remaining tools
//! if a steering message is pending (Pi's "skipped due to steering" pattern).
//!
//! ## Observability
//!
//! Draining and injecting a steering message is an implicit behavior that
//! must be observable. The `AgentEvent::SteeringMessageApplied` event
//! records the injection action in the JSONL so it can be replayed.

use serde::{Deserialize, Serialize};

/// A user-authored steering message, queued for injection into the
/// running agent loop.
///
/// Created by the UI/CLI layer (e.g. user types `/steer wait, use Python
/// instead of Rust` while the agent is running) and sent via the
/// `mpsc::UnboundedSender<SteeringMessage>` held by the caller. The
/// `AgentLoop` drains pending messages at the next turn boundary and
/// injects them as `AgentMessage::user(content)` into the conversation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SteeringMessage {
    /// The user's steering text. Will become the content of a User-role
    /// `AgentMessage` when injected.
    pub content: String,
    /// Unix millisecond timestamp when the message was enqueued.
    /// For observability — lets the JSONL reader see how long the
    /// steering message waited before being applied.
    #[serde(default)]
    pub enqueued_at: u64,
}

impl SteeringMessage {
    /// Create a new steering message with the given content, stamping
    /// `enqueued_at` with the current time.
    pub fn new(content: impl Into<String>) -> Self {
        Self { content: content.into(), enqueued_at: now_ms() }
    }

    /// Create a steering message with an explicit enqueue timestamp
    /// (for testing / replay).
    pub fn with_timestamp(content: impl Into<String>, enqueued_at: u64) -> Self {
        Self { content: content.into(), enqueued_at }
    }
}

/// Current Unix time in milliseconds. Returns 0 if the system clock is
/// unavailable (extremely rare; primarily a testing convenience).
fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn steering_message_new_stamps_time() {
        let msg = SteeringMessage::new("use Python instead");
        assert_eq!(msg.content, "use Python instead");
        // enqueued_at should be non-zero (current time)
        assert!(msg.enqueued_at > 0, "enqueued_at should be stamped");
    }

    #[test]
    fn steering_message_with_explicit_timestamp() {
        let msg = SteeringMessage::with_timestamp("test", 12345);
        assert_eq!(msg.content, "test");
        assert_eq!(msg.enqueued_at, 12345);
    }

    #[test]
    fn steering_message_serde_roundtrip() {
        let msg = SteeringMessage::with_timestamp("roundtrip test", 99999);
        let json = serde_json::to_string(&msg).unwrap();
        let decoded: SteeringMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.content, "roundtrip test");
        assert_eq!(decoded.enqueued_at, 99999);
    }

    #[test]
    fn steering_message_default_timestamp_serde() {
        // When deserializing without enqueued_at, defaults to 0
        let json = r#"{"content":"no timestamp"}"#;
        let msg: SteeringMessage = serde_json::from_str(json).unwrap();
        assert_eq!(msg.content, "no timestamp");
        assert_eq!(msg.enqueued_at, 0);
    }
}

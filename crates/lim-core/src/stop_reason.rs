use serde::{Deserialize, Serialize};

/// Reason the agent loop or LLM turn stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    /// Model finished generating (natural end of turn).
    EndTurn,
    /// Model wants to invoke one or more tools.
    ToolCall,
    /// Hit the max token limit.
    MaxTokens,
    /// The operation was aborted (user cancel or timeout).
    Aborted,
    /// An error occurred during generation.
    Error,
}

impl std::fmt::Display for StopReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EndTurn => write!(f, "end_turn"),
            Self::ToolCall => write!(f, "tool_call"),
            Self::MaxTokens => write!(f, "max_tokens"),
            Self::Aborted => write!(f, "aborted"),
            Self::Error => write!(f, "error"),
        }
    }
}

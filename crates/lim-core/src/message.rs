use serde::{Deserialize, Serialize};

use crate::content::ContentBlock;
use crate::id::{MessageId, ToolCallId};
use crate::stop_reason::StopReason;
use crate::usage::Usage;

/// Role of a message participant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    /// System instructions (invisible to end user).
    System,
    /// End user input.
    User,
    /// LLM assistant output.
    Assistant,
    /// Tool execution result fed back to the model.
    Tool,
}

/// Internal message representation used throughout the agent runtime.
///
/// This is the unified message type for UI, session storage, and event logging.
/// It is NOT sent directly to the LLM — use [`convert_to_llm`] to produce
/// [`LlmMessage`] for the provider.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentMessage {
    /// Unique message identifier.
    pub id: MessageId,
    /// Who produced this message.
    pub role: Role,
    /// Ordered content blocks.
    pub content: Vec<ContentBlock>,
    /// Associated tool call IDs (for Tool role messages).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_call_ids: Vec<ToolCallId>,
    /// Whether this tool result represents an error (only meaningful for Tool role).
    #[serde(default, skip_serializing_if = "is_false")]
    pub is_error: bool,
}

fn is_false(b: &bool) -> bool {
    !b
}

impl AgentMessage {
    /// Create a new user message with text content.
    pub fn user(text: impl Into<String>) -> Self {
        Self {
            id: MessageId::new(),
            role: Role::User,
            content: vec![ContentBlock::text(text)],
            tool_call_ids: Vec::new(),
            is_error: false,
        }
    }

    /// Create a new assistant message with the given content blocks.
    pub fn assistant(content: Vec<ContentBlock>) -> Self {
        Self {
            id: MessageId::new(),
            role: Role::Assistant,
            content,
            tool_call_ids: Vec::new(),
            is_error: false,
        }
    }

    /// Create a tool result message.
    pub fn tool_result(tool_call_id: ToolCallId, output: impl Into<String>, is_error: bool) -> Self {
        Self {
            id: MessageId::new(),
            role: Role::Tool,
            content: vec![ContentBlock::text(output)],
            tool_call_ids: vec![tool_call_id],
            is_error,
        }
    }

    /// Create a system message.
    pub fn system(text: impl Into<String>) -> Self {
        Self {
            id: MessageId::new(),
            role: Role::System,
            content: vec![ContentBlock::text(text)],
            tool_call_ids: Vec::new(),
            is_error: false,
        }
    }

    /// Extract all tool calls from this message's content blocks.
    pub fn tool_calls(&self) -> Vec<(ToolCallId, &str, &serde_json::Value)> {
        self.content
            .iter()
            .filter_map(|block| {
                if let ContentBlock::ToolCall {
                    id,
                    name,
                    arguments,
                } = block
                {
                    Some((id.clone(), name.as_str(), arguments))
                } else {
                    None
                }
            })
            .collect()
    }

    /// Check if this message contains any tool calls.
    pub fn has_tool_calls(&self) -> bool {
        self.content.iter().any(|b| b.is_tool_call())
    }
}

/// Linearized message view for LLM consumption.
///
/// Produced by filtering [`AgentMessage`] via `convert_to_llm()`:
/// only System, User, Assistant, and Tool messages are included;
/// non-LLM content (notifications, artifacts, summaries) is excluded.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmMessage {
    pub role: Role,
    pub content: Vec<ContentBlock>,
    /// For Tool role: which tool call(s) this result answers.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_call_ids: Vec<ToolCallId>,
}

impl LlmMessage {
    /// Convert an [`AgentMessage`] to an [`LlmMessage`] if it has an LLM-visible role.
    pub fn from_agent_message(msg: &AgentMessage) -> Option<Self> {
        match msg.role {
            Role::System | Role::User | Role::Assistant | Role::Tool => Some(Self {
                role: msg.role,
                content: msg.content.clone(),
                tool_call_ids: msg.tool_call_ids.clone(),
            }),
        }
    }
}

/// Convert a sequence of agent messages to LLM messages.
///
/// This is the "最晚转换" (latest conversion) — only applied right before
/// sending to the provider, preserving the full agent message history for
/// UI, session, and replay purposes.
pub fn convert_to_llm(messages: &[AgentMessage]) -> Vec<LlmMessage> {
    messages
        .iter()
        .filter_map(LlmMessage::from_agent_message)
        .collect()
}

/// Metadata attached to a completed assistant turn.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TurnResult {
    /// Why the turn ended.
    pub stop_reason: StopReason,
    /// Token usage for this turn.
    pub usage: Usage,
    /// The assistant message produced.
    pub message_id: MessageId,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::content::ContentBlock;

    #[test]
    fn user_message() {
        let msg = AgentMessage::user("hello");
        assert_eq!(msg.role, Role::User);
        assert_eq!(msg.content.len(), 1);
    }

    #[test]
    fn assistant_with_tool_call() {
        let tc_id = ToolCallId::new();
        let msg = AgentMessage::assistant(vec![
            ContentBlock::text("Let me check"),
            ContentBlock::tool_call(tc_id.clone(), "bash", serde_json::json!({"cmd": "ls"})),
        ]);
        assert!(msg.has_tool_calls());
        let calls = msg.tool_calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, tc_id);
    }

    #[test]
    fn convert_filters_non_llm() {
        let messages = vec![
            AgentMessage::system("you are helpful"),
            AgentMessage::user("hi"),
            AgentMessage::assistant(vec![ContentBlock::text("hello!")]),
        ];
        let llm_msgs = convert_to_llm(&messages);
        assert_eq!(llm_msgs.len(), 3);
        assert_eq!(llm_msgs[0].role, Role::System);
    }
}

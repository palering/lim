use serde::{Deserialize, Serialize};

use crate::id::ToolCallId;

/// Category of a tool, used by [`ApprovalMode`](../../lim_tui/approval/enum.ApprovalMode.html)
/// to decide whether a tool call requires user approval.
///
/// Tools declare their category via [`Tool::category`](../../lim_agent/tool/trait.Tool.html#method.category).
/// The default category is `ReadOnly` — tools that modify state or execute
/// commands should override.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ToolCategory {
    /// Read-only tools (read, glob, grep, ls). Never modify state.
    ReadOnly,
    /// Write tools (write, edit). Modify files within workspace.
    Write,
    /// Script execution tools (bash). Can execute arbitrary commands.
    Script,
    /// Network access tools (fetch, web_search).
    Network,
    /// Remote tools (MCP, external services). Phase 4.
    Remote,
}

impl ToolCategory {
    /// Whether this category is considered "safe" (no state mutation, no
    /// arbitrary code execution, no network).
    pub fn is_read_only(self) -> bool {
        matches!(self, Self::ReadOnly)
    }
}

/// Schema definition for a tool that the agent can invoke.
///
/// This is sent to the LLM as part of the tool definitions, so it can
/// generate valid tool calls.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolSchema {
    /// Tool name (unique identifier, e.g. "shell", "read", "write").
    pub name: String,
    /// Human-readable description of what the tool does.
    pub description: String,
    /// JSON Schema for the tool's input parameters.
    /// The LLM uses this to generate valid arguments.
    pub parameters: serde_json::Value,
}

impl ToolSchema {
    /// Create a new tool schema.
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        parameters: serde_json::Value,
    ) -> Self {
        Self { name: name.into(), description: description.into(), parameters }
    }
}

/// A tool call request, as parsed from the model's output.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    /// Unique identifier for this call.
    pub id: ToolCallId,
    /// Name of the tool to invoke.
    pub name: String,
    /// Raw JSON arguments. Validated against [`ToolSchema::parameters`]
    /// before execution; only deserialized to typed struct at that point.
    pub arguments: serde_json::Value,
}

impl ToolCall {
    /// Create a new tool call.
    pub fn new(id: ToolCallId, name: impl Into<String>, arguments: serde_json::Value) -> Self {
        Self { id, name: name.into(), arguments }
    }
}

/// The result of a tool execution, fed back to the LLM.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResult {
    /// The tool call this result corresponds to.
    pub tool_call_id: ToolCallId,
    /// Text output of the tool (stdout, file content, error message, etc.).
    pub output: String,
    /// Whether the tool execution resulted in an error.
    pub is_error: bool,
}

impl ToolResult {
    /// Create a successful tool result.
    pub fn success(tool_call_id: ToolCallId, output: impl Into<String>) -> Self {
        Self { tool_call_id, output: output.into(), is_error: false }
    }

    /// Create an error tool result.
    pub fn error(tool_call_id: ToolCallId, output: impl Into<String>) -> Self {
        Self { tool_call_id, output: output.into(), is_error: true }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_result_success() {
        let id = ToolCallId::new();
        let result = ToolResult::success(id.clone(), "file contents here");
        assert!(!result.is_error);
        assert_eq!(result.tool_call_id, id);
    }

    #[test]
    fn tool_result_error() {
        let id = ToolCallId::new();
        let result = ToolResult::error(id, "permission denied");
        assert!(result.is_error);
    }

    #[test]
    fn tool_schema_serde() {
        let schema = ToolSchema::new(
            "bash",
            "Execute a shell command",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "command": { "type": "string" }
                },
                "required": ["command"]
            }),
        );
        let json = serde_json::to_string(&schema).unwrap();
        let parsed: ToolSchema = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.name, "bash");
    }
}

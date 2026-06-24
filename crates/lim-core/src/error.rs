use thiserror::Error;

use crate::id::ToolCallId;

/// Core error type for the lim runtime.
#[derive(Debug, Error)]
pub enum LimError {
    /// LLM provider returned an error.
    #[error("provider error: {0}")]
    Provider(String),

    /// Tool execution failed.
    #[error("tool `{name}` failed: {message}")]
    ToolExecution { name: String, message: String },

    /// Tool schema validation failed.
    #[error("invalid tool arguments for `{tool_name}`: {reason}")]
    ToolSchemaValidation { tool_name: String, reason: String },

    /// Tool call references an unknown tool.
    #[error("unknown tool: {0}")]
    UnknownTool(String),

    /// The tool call ID was not found or already consumed.
    #[error("unknown tool call id: {0}")]
    UnknownToolCall(ToolCallId),

    /// Agent was cancelled by the user or system.
    #[error("operation cancelled")]
    Cancelled,

    /// Session or event log I/O error.
    #[error("session I/O error: {0}")]
    SessionIo(String),

    /// Configuration error.
    #[error("configuration error: {0}")]
    Config(String),

    /// Catch-all for unexpected errors.
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

/// Crate-level Result alias.
pub type Result<T> = std::result::Result<T, LimError>;

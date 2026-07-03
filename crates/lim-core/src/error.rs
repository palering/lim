use thiserror::Error;

use crate::id::ToolCallId;

/// Core error type for the lim runtime.
///
/// ## Design
///
/// - **Closed enum**: no catch-all `Other(anyhow::Error)`. New error sources
///   must add a variant or map to an existing one explicitly.
/// - **Source chain**: variants wrapping typed external errors preserve them
///   via `#[source]`, so `Error::source()` walks back to the root cause.
/// - **String for opaque external errors**: `Provider(String)` and
///   `Config(String)` keep the error as a string because the underlying typed
///   error lives in a crate that `lim-core` cannot depend on (`lim-llm`'s
///   `ProviderError`) or comes from too many heterogeneous sources (toml,
///   io, custom validation). Callers that need structured access to provider
///   errors should inspect `ProviderError` before it crosses this boundary.
#[derive(Debug, Error)]
pub enum LimError {
    /// LLM provider error.
    ///
    /// The string carries the provider's human-readable error message.
    /// Structured `ProviderError` details (rate limit, auth, context window,
    /// etc.) are flattened because `lim-core` cannot depend on `lim-llm`
    /// (architectural layering). Callers needing retry decisions should
    /// inspect `ProviderError` directly from the provider stream.
    #[error("provider error: {0}")]
    Provider(String),

    /// Tool execution failed.
    ///
    /// `message` is a human-readable summary (may include the source's
    /// Display for user-facing context); `source` preserves the underlying
    /// typed error (io, serde_json, process exit, etc.) so `Error::source()`
    /// can walk back to the root cause for debugging.
    #[error("tool `{name}` failed: {message}")]
    ToolExecution {
        name: String,
        message: String,
        #[source]
        source: Option<Box<dyn std::error::Error + Send + Sync>>,
    },

    /// Tool schema validation failed.
    #[error("invalid tool arguments for `{tool_name}`: {reason}")]
    ToolSchemaValidation { tool_name: String, reason: String },

    /// Tool call references an unknown tool.
    #[error("unknown tool: {0}")]
    UnknownTool(String),

    /// Attempted to register a tool with a name already in the registry.
    ///
    /// Returned by `ToolRegistry::register` when a tool name collides with an
    /// existing registration. The registry is append-only within a session;
    /// to replace a tool, call `unregister` first.
    #[error("tool already registered: {0}")]
    ToolAlreadyRegistered(String),

    /// The tool call ID was not found or already consumed.
    #[error("unknown tool call id: {0}")]
    UnknownToolCall(ToolCallId),

    /// Agent was cancelled by the user or system.
    #[error("operation cancelled")]
    Cancelled,

    /// Session or event log I/O error.
    ///
    /// `context` describes what the code was trying to do; `source` preserves
    /// the underlying error (typically `std::io::Error` or `serde_json::Error`).
    #[error("session I/O error: {context}")]
    SessionIo {
        context: String,
        #[source]
        source: Option<Box<dyn std::error::Error + Send + Sync>>,
    },

    /// Configuration error.
    ///
    /// Carries a human-readable message. Sources are heterogeneous (toml
    /// parse errors, missing fields, io errors) so we keep this as a String;
    /// callers needing the typed source should handle config loading errors
    /// before they cross into `LimError`.
    #[error("configuration error: {0}")]
    Config(String),

    /// Terminal (TUI) I/O error.
    ///
    /// `context` describes what the TUI was doing (init, render, input read);
    /// `source` preserves the underlying `io::Error` from crossterm/ratatui.
    #[error("terminal error: {context}")]
    Terminal {
        context: String,
        #[source]
        source: Option<Box<dyn std::error::Error + Send + Sync>>,
    },
}

/// Type alias for results returning `LimError`.
pub type Result<T> = std::result::Result<T, LimError>;

impl LimError {
    /// Create a `ToolExecution` error with an underlying source.
    ///
    /// Use this when wrapping a typed error (io, serde, process). The source
    /// is preserved so `Error::source()` can walk back to the root cause.
    pub fn tool_execution(
        name: impl Into<String>,
        message: impl Into<String>,
        source: impl Into<Box<dyn std::error::Error + Send + Sync>>,
    ) -> Self {
        Self::ToolExecution {
            name: name.into(),
            message: message.into(),
            source: Some(source.into()),
        }
    }

    /// Create a `ToolExecution` error with only a message (no source).
    ///
    /// Use this when the tool itself defines the error (e.g. a tool returns
    /// its own error message) and there is no underlying typed error to wrap.
    pub fn tool_execution_msg(name: impl Into<String>, message: impl Into<String>) -> Self {
        Self::ToolExecution { name: name.into(), message: message.into(), source: None }
    }

    /// Create a `SessionIo` error with an underlying source.
    ///
    /// Use this when wrapping a typed io/serde error from session file
    /// operations.
    pub fn session_io(
        context: impl Into<String>,
        source: impl Into<Box<dyn std::error::Error + Send + Sync>>,
    ) -> Self {
        Self::SessionIo { context: context.into(), source: Some(source.into()) }
    }

    /// Create a `SessionIo` error with only a context message (no source).
    pub fn session_io_msg(context: impl Into<String>) -> Self {
        Self::SessionIo { context: context.into(), source: None }
    }

    /// Create a `Terminal` error with an underlying source.
    ///
    /// Use this when wrapping a typed io error from crossterm/ratatui.
    pub fn terminal(
        context: impl Into<String>,
        source: impl Into<Box<dyn std::error::Error + Send + Sync>>,
    ) -> Self {
        Self::Terminal { context: context.into(), source: Some(source.into()) }
    }

    /// Create a `Terminal` error with only a context message (no source).
    pub fn terminal_msg(context: impl Into<String>) -> Self {
        Self::Terminal { context: context.into(), source: None }
    }
}

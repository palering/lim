use std::future::Future;
use std::pin::Pin;

use tokio_util::sync::CancellationToken;

use lim_core::error::Result;
use lim_core::tool::{ToolResult, ToolSchema};

/// Boxed future type for async tool execution (dyn-compatible).
pub type ToolFuture<T> = Pin<Box<dyn Future<Output = T> + Send>>;

/// Trait for agent-invocable tools.
///
/// Each tool has a name (unique within a registry), a JSON Schema describing
/// its input parameters, and an async execute method.
pub trait Tool: Send + Sync {
    /// Unique name for this tool (e.g., "read", "write", "bash").
    fn name(&self) -> &str;

    /// Schema sent to the LLM so it can generate valid tool calls.
    fn schema(&self) -> ToolSchema;

    /// Execute the tool with validated arguments.
    ///
    /// The `cancel` token is derived from the agent loop's token and should
    /// be forwarded to any long-running operations (e.g., shell commands).
    fn execute(
        &self,
        tool_call_id: lim_core::ToolCallId,
        args: serde_json::Value,
        cancel: CancellationToken,
    ) -> ToolFuture<Result<ToolResult>>;
}

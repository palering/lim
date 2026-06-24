use std::future::Future;
use std::pin::Pin;

use lim_core::error::Result;
use lim_core::tool::{ToolCall, ToolResult};

/// Boxed future type for async hook execution (dyn-compatible).
pub type HookFuture<T> = Pin<Box<dyn Future<Output = T> + Send>>;

/// Decision returned by `before_tool_call` hook.
#[derive(Debug)]
pub enum BeforeToolDecision {
    /// Proceed with the tool call as-is.
    Proceed,
    /// Replace the tool call arguments before execution.
    ModifyArgs(serde_json::Value),
    /// Reject the tool call with a reason (fed back as error result).
    Reject(String),
}

/// Hooks for intercepting tool calls in the agent loop.
pub trait AgentHooks: Send + Sync {
    /// Called before a tool is executed.
    fn before_tool_call(
        &self,
        call: &ToolCall,
    ) -> HookFuture<Result<BeforeToolDecision>>;

    /// Called after a tool has executed.
    fn after_tool_call(
        &self,
        result: &ToolResult,
    ) -> HookFuture<Result<()>>;
}

use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use lim_core::context_edit::ContextEdit;
use lim_core::error::Result;
pub use lim_core::loop_end::SessionEndReason;
use lim_core::message::AgentMessage;
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

/// Context passed to `AgentHooks::should_stop` after each turn.
///
/// Carries cumulative stats for the current session so hooks can make
/// informed stop decisions (e.g. budget exhaustion, no-progress detection).
#[derive(Debug, Clone)]
pub struct StopContext<'a> {
    /// 0-based turn number that just completed (0 = after first turn).
    pub turn_count: usize,
    /// Cumulative input + output tokens across all turns in this session.
    pub total_tokens: u64,
    /// Wall-clock elapsed since the session started.
    pub elapsed: Duration,
    /// Results from the last turn's tool calls (empty if no tools ran).
    pub last_tool_results: &'a [ToolResult],
}

/// Decision returned by `should_stop` after each turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopDecision {
    /// Continue the turn loop (default).
    Continue,
    /// End the current session with the given reason.
    StopSession(SessionEndReason),
}

/// Hooks for intercepting tool calls, editing context, and controlling
/// the turn loop.
///
/// This is lim's **unified policy extension point**. All strategy decisions
/// (tool-call interception, context editing, stop conditions) are injected
/// through this trait. Methods have default implementations so consumers
/// override only what they need.
///
/// See `docs/architecture/layers.md` for the full extension-point catalog
/// and `docs/architecture/loop-engineering.md` for Loop Engineering usage.
pub trait AgentHooks: Send + Sync {
    /// Called before a tool is executed.
    fn before_tool_call(&self, call: &ToolCall) -> HookFuture<Result<BeforeToolDecision>>;

    /// Called after a tool has executed.
    fn after_tool_call(&self, result: &ToolResult) -> HookFuture<Result<()>>;

    /// Called after tool results are collected, before the next LLM call.
    ///
    /// Return `Ok(Some(ContextEdit))` to modify the message list sent to the LLM.
    /// Return `Ok(None)` to skip editing (default behavior).
    /// Return `Err(...)` to abort the turn.
    ///
    /// This hook fires on every turn. Edits are transient — they affect only
    /// the current LLM request, not the persisted session history.
    fn edit_context(&self, _messages: &[AgentMessage]) -> HookFuture<Result<Option<ContextEdit>>> {
        Box::pin(std::future::ready(Ok(None)))
    }

    /// Called after each turn completes. Return `StopSession` to end the
    /// current session (the `LoopRunner` will then verify the goal and
    /// decide whether to start a new session).
    ///
    /// Default: always continue. Loop Engineering consumers override this
    /// to implement budget limits, no-progress detection, etc.
    fn should_stop(&self, _ctx: &StopContext<'_>) -> StopDecision {
        StopDecision::Continue
    }

    /// Hard upper bound on turns within a single session. `None` = unlimited
    /// (only the `AgentConfig::max_turns` safety valve applies).
    ///
    /// `AgentLoop` checks this before starting each turn. When exceeded, the
    /// session ends with `SessionEndReason::MaxStepsReached`.
    fn max_steps(&self) -> Option<usize> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A no-op hooks impl used by tests that need an `Arc<dyn AgentHooks>`
    /// but don't want any interception. All methods return defaults.
    struct NoopHooks;

    impl AgentHooks for NoopHooks {
        fn before_tool_call(&self, _call: &ToolCall) -> HookFuture<Result<BeforeToolDecision>> {
            Box::pin(std::future::ready(Ok(BeforeToolDecision::Proceed)))
        }
        fn after_tool_call(&self, _result: &ToolResult) -> HookFuture<Result<()>> {
            Box::pin(std::future::ready(Ok(())))
        }
    }

    #[test]
    fn default_should_stop_continues() {
        let hooks = NoopHooks;
        let ctx = StopContext {
            turn_count: 100,
            total_tokens: 999_999,
            elapsed: Duration::from_secs(3600),
            last_tool_results: &[],
        };
        assert_eq!(hooks.should_stop(&ctx), StopDecision::Continue);
    }

    #[test]
    fn default_max_steps_is_none() {
        let hooks = NoopHooks;
        assert_eq!(hooks.max_steps(), None);
    }
}

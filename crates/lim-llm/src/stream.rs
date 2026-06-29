use serde::{Deserialize, Serialize};

use lim_core::id::ToolCallId;
use lim_core::stop_reason::StopReason;
use lim_core::usage::Usage;

/// A single streaming event from an LLM provider.
///
/// Providers emit a sequence of these events during generation.
/// The agent loop consumes them to build up content blocks incrementally.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum StreamEvent {
    /// Incremental text content from the assistant.
    TextDelta { text: String },

    /// Incremental thinking/reasoning content.
    ThinkingDelta { text: String },

    /// A new tool call is starting.
    ///
    /// `name` may be `None` if the provider hasn't sent the function name yet
    /// (e.g. OpenAI Chat Completions sometimes sends `id` and `name` in the
    /// same delta, but some compatible providers split them). The accumulator
    /// will accept a second `ToolCallStart` with the same `id` to fill in the
    /// name if it was initially `None`.
    ToolCallStart {
        id: ToolCallId,
        name: Option<String>,
    },

    /// Incremental JSON argument fragment for an in-progress tool call.
    ToolCallDelta {
        id: ToolCallId,
        /// Partial JSON string to be accumulated.
        arguments_delta: String,
    },

    /// Token usage information (may arrive mid-stream or at end).
    Usage(Usage),

    /// Generation is complete.
    Done { stop_reason: StopReason },
}
